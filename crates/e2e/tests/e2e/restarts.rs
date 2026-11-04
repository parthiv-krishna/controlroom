//! Restarts mid-call: the host daemon (its shell survives: the result is recovered from the
//! shell's log), a host "reboot" (the shell died too: `lost`, and the shell is reopened with a
//! reset notice), and the agent daemon (the extension resends the call; it doesn't run again
//! and its answer is delivered once).

use controlroom_e2e::held::Held;
use controlroom_e2e::script::{bash, say, scenario};
use controlroom_e2e::world::World;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_daemon_restart_mid_bash_recovers_the_result() {
    let mut world = World::start().await;
    let work = world.devbox.dir("work");
    let held = Held::new(&work, "test");
    world.agentd.scenario(&scenario(vec![
        bash("c_env", "devbox", "devbox", "export STAGE=two"),
        bash(
            "c_bash",
            "devbox",
            "devbox",
            &held.command("echo testing", "echo passed"),
        ),
        bash("c_after", "devbox", "devbox", "echo \"[$STAGE]\""),
        say("Tested."),
    ]));
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/work")], json!({}))
        .await;

    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "run the tests").await;
    held.started().await;
    world.devbox.kill_daemon();
    // The command finishes while no daemon runs; the next connection starts a new one.
    held.release().await;
    world.wait_turn(&id, mark).await;

    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "every call ended", |t| {
            t.tool_done("c_after")
        })
        .await;
    let result = timeline.tool_json("c_bash");
    assert_eq!(result["result"]["output"], "testing\npassed", "{result}");
    assert_eq!(result["result"]["exit_code"], 0);
    assert_eq!(held.runs(), 1);
    // The shell survived the daemon: its variables are still there, without a reset.
    let after = &timeline.tool_json("c_after")["result"];
    assert_eq!(after["output"], "[two]", "{after}");
    assert!(after["shell_reset"].is_null(), "{after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_reboot_mid_bash_loses_the_call_and_resets_the_shell() {
    let mut world = World::start().await;
    let work = world.devbox.dir("work");
    std::fs::create_dir_all(work.join("sub")).unwrap();
    let held = Held::new(&work, "migrate");
    world.agentd.scenario(&scenario(vec![
        bash("c_env", "devbox", "devbox", "cd sub && export STAGE=two"),
        bash(
            "c_bash",
            "devbox",
            "devbox",
            &held.command("echo migrating", "echo migrated"),
        ),
        bash("c_after", "devbox", "devbox", "pwd; echo \"[$STAGE]\""),
        say("Checked."),
    ]));
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/work")], json!({}))
        .await;

    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "migrate").await;
    held.started().await;
    // Everything on the host dies, the shell and the command with it.
    world.devbox.reboot();
    world.wait_turn(&id, mark).await;

    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "every call ended", |t| {
            t.tool_done("c_after")
        })
        .await;
    assert!(timeline.tool_failed("c_bash"));
    let error = timeline.tool_json("c_bash");
    assert_eq!(error["data"]["kind"], "lost", "{error}");
    assert_eq!(error["data"]["may_have_run"], true);
    assert_eq!(held.runs(), 1, "never rerun");
    // The shell is reopened in its last directory, without its variables, and says so.
    let after = &timeline.tool_json("c_after")["result"];
    assert_eq!(
        after["output"],
        format!("{}\n[]", work.join("sub").display()),
        "{after}"
    );
    assert!(after["shell_reset"].is_string(), "{after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agentd_restart_mid_call_delivers_the_answer_once() {
    let mut world = World::start().await;
    let work = world.buildbox.dir("work");
    let held = Held::new(&work, "package");
    world.agentd.scenario(&scenario(vec![
        bash(
            "c_bash",
            "buildbox",
            "buildbox",
            &held.command("echo packing", "echo packed"),
        ),
        say("Packaged."),
    ]));
    let buildbox = world.buildbox.id.clone();
    let id = world
        .create_session(&[(&buildbox, "~/work")], json!({}))
        .await;

    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "package it").await;
    held.started().await;
    world.agentd.kill();
    // The command finishes while the agent daemon is down.
    held.release().await;
    world.agentd.start().await;
    world.wait_turn(&id, mark).await;

    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the bash result", |t| t.tool_done("c_bash"))
        .await;
    assert!(
        !timeline.tool_failed("c_bash"),
        "{:?}",
        timeline.tool("c_bash")
    );
    let result = timeline.tool_json("c_bash");
    assert_eq!(result["result"]["output"], "packing\npacked", "{result}");
    assert_eq!(held.runs(), 1, "the call didn't run again");
    assert_eq!(timeline.of_kind("tool_call").len(), 1);
    assert!(timeline.injected_runs("tool_late").is_empty());
}
