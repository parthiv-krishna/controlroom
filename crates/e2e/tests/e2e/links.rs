//! Link loss pauses, it doesn't fail: a command running when the link drops finishes on the host
//! and its result reaches the agent once, within the grace; past the grace the call fails with
//! `connection_lost` and its late result is injected once after the reconnect.

use controlroom_e2e::held::Held;
use controlroom_e2e::script::{bash, say, scenario};
use controlroom_e2e::world::{Which, World};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_drop_within_the_grace_delivers_the_result_once() {
    let mut world = World::start().await;
    let work = world.devbox.dir("work");
    let held = Held::new(&work, "build");
    world.agentd.scenario(&scenario(vec![
        bash(
            "c_bash",
            "devbox",
            "devbox",
            &held.command("echo before", "echo after"),
        ),
        say("Built."),
    ]));
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/work")], json!({}))
        .await;

    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "build it").await;
    held.started().await;
    world.cut(Which::Devbox).await;
    world.wait_availability(&id, &devbox, false).await;
    // The command finishes on the host while the link is down.
    held.release().await;
    world.restore(Which::Devbox).await;
    world.wait_turn(&id, mark).await;

    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the bash result", |t| t.tool_done("c_bash"))
        .await;
    let result = timeline.tool_json("c_bash");
    assert_eq!(result["link_drops"], 1, "{result}");
    assert_eq!(result["result"]["output"], "before\nafter");
    assert_eq!(result["result"]["exit_code"], 0);
    assert_eq!(held.runs(), 1, "the command ran once");
    assert_eq!(timeline.of_kind("tool_call").len(), 1);
    assert!(timeline.injected_runs("tool_late").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn past_the_grace_the_call_fails_and_its_late_result_is_injected_once() {
    let mut world = World::start().await;
    let work = world.devbox.dir("work");
    let held = Held::new(&work, "deploy");
    world.agentd.scenario(&scenario(vec![
        bash(
            "c_bash",
            "devbox",
            "devbox",
            &held.command("echo deploying", "echo deployed-42"),
        ),
        say("Waiting for devbox."),
        say("Noted the late result."),
        say("Still here."),
    ]));
    let devbox = world.devbox.id.clone();
    world
        .ui
        .ok(
            "machines.set_settings",
            json!({"machine_id": devbox, "grace_s": 1}),
        )
        .await;
    let id = world
        .create_session(&[(&devbox, "~/work")], json!({}))
        .await;

    // The link stays down past the grace: the call fails with `connection_lost` and the run
    // ends.
    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "deploy").await;
    held.started().await;
    world.cut(Which::Devbox).await;
    let mark = {
        let summary = world.wait_turn(&id, mark).await;
        summary["last_turn_end_seq"].as_u64().unwrap()
    };
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the failed call", |t| t.tool_done("c_bash"))
        .await;
    assert!(timeline.tool_failed("c_bash"));
    let error = timeline.tool_json("c_bash");
    assert_eq!(error["data"]["kind"], "connection_lost", "{error}");
    assert_eq!(error["data"]["machine_id"], devbox.as_str());

    // The command finishes on the host meanwhile; after the reconnect its result is injected.
    held.release().await;
    world.restore(Which::Devbox).await;
    world.wait_turn(&id, mark).await;
    world
        .until(&mut timeline, "the late result's run", |t| {
            !t.injected_runs("tool_late").is_empty()
        })
        .await;
    let late: Vec<String> = timeline
        .user_texts()
        .into_iter()
        .filter(|t| t.contains("deployed-42"))
        .collect();
    assert_eq!(late.len(), 1, "{late:?}");

    // Another turn: the late result was injected once, and the command ran once.
    world.prompt(&id, "anything else?").await;
    world
        .until(&mut timeline, "the next run", |t| {
            t.user_texts().iter().any(|u| u == "anything else?")
        })
        .await;
    assert_eq!(timeline.injected_runs("tool_late").len(), 1);
    assert_eq!(
        timeline
            .user_texts()
            .iter()
            .filter(|t| t.contains("deployed-42"))
            .count(),
        1
    );
    assert_eq!(held.runs(), 1);
}
