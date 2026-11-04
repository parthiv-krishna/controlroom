//! A background job across a link cut and across an agent daemon restart: a retried run is the
//! same job, the matches produced during the outage wake the agent once (merged), and the exit
//! wakes it exactly once although it happened while the agent daemon was down.

use controlroom_controller::test_support::op;
use controlroom_e2e::held::Held;
use controlroom_e2e::script::{say, scenario, tool};
use controlroom_e2e::session::Timeline;
use controlroom_e2e::world::{Which, World};
use serde_json::{Value, json};

/// The job-origin runs whose label mentions `what` (e.g. `build (exit 0)`).
fn wakes<'a>(timeline: &'a Timeline, what: &str) -> Vec<&'a Value> {
    timeline
        .injected_runs("job")
        .into_iter()
        .filter(|r| {
            r["trigger"]["label"]
                .as_str()
                .is_some_and(|l| l.contains(what))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_wakes_once_across_a_link_cut_and_an_agentd_restart() {
    let mut world = World::start().await;
    let app = world.devbox.dir("app");
    let (first, second) = (Held::new(&app, "first"), Held::new(&app, "second"));
    let command = format!(
        "{}; {}",
        first.command("echo start", "printf 'ERROR one\\nERROR two\\n'"),
        second.command("true", "echo finished")
    );
    let run = |call: &str, command: &str, name: &str| {
        (
            "job.run",
            json!({"call_id": call, "command": command, "machine": "devbox", "name": name}),
        )
    };
    world.agentd.scenario(&scenario(vec![
        tool("c_job", "job", &[run("c_job", &command, "build")]),
        // The extension resends a run with its call id (e.g. after a reconnect).
        tool("c_again", "job", &[run("c_job", &command, "build")]),
        say("Started the build."),
        say("Saw the errors."),
        say("The build finished."),
        tool("c_quick", "job", &[run("c_quick", "echo quick", "quick")]),
        say("Started a quick job."),
    ]));
    let devbox = world.devbox.id.clone();
    let id = world.create_session(&[(&devbox, "~/app")], json!({})).await;

    world.prompt(&id, "build").await;
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "both runs answered", |t| {
            t.tool_done("c_again")
        })
        .await;
    let job = timeline.tool_json("c_job");
    assert_eq!(timeline.tool_json("c_again"), job, "the same job");
    let job_id = job["job_id"].as_str().unwrap().to_owned();
    first.started().await;
    assert_eq!(first.runs(), 1, "one job ran");
    world
        .ui
        .ok(
            "job.follow",
            json!({"op_id": op(), "id": id, "job_id": job_id, "pattern": "ERROR",
                   "buffer_seconds": 2}),
        )
        .await;

    // The link is cut while the job prints two matching lines.
    world.cut(Which::Devbox).await;
    world.wait_availability(&id, &devbox, false).await;
    first.release().await;
    second.started().await;
    world.restore(Which::Devbox).await;
    world
        .until(&mut timeline, "the follow wake", |t| {
            !wakes(t, "build (follow)").is_empty()
        })
        .await;
    let follow = job_messages(&timeline, "is still running");
    assert_eq!(follow.len(), 1, "one merged follow wake: {follow:#?}");
    assert!(
        follow[0].contains("2 lines matching /ERROR/ at lines 2-3"),
        "{}",
        follow[0]
    );

    // The job exits while the agent daemon is down.
    let mark = world.turn_mark(&id);
    world.agentd.kill();
    second.release().await;
    world.agentd.start().await;
    world
        .until(&mut timeline, "the exit wake", |t| {
            !wakes(t, "build (exit 0)").is_empty()
        })
        .await;
    world.wait_turn(&id, mark).await;

    // Another job's exit wake comes later in the host's event log: once it is in, every
    // earlier event has been consumed, so a replayed wake would show by now. (It arrives as a
    // steer of the running turn.)
    world.prompt(&id, "one more").await;
    world
        .until(&mut timeline, "the quick job's exit wake", |t| {
            !job_messages(t, "`quick` exited").is_empty()
        })
        .await;
    assert_eq!(wakes(&timeline, "build (exit 0)").len(), 1, "one exit run");
    assert_eq!(
        wakes(&timeline, "build (follow)").len(),
        1,
        "one follow run"
    );
    let build = format!("job {job_id} `build`");
    assert_eq!(
        job_messages(&timeline, &build).len(),
        2,
        "two wakes for the build: {:#?}",
        job_messages(&timeline, &build)
    );
    let exits = job_messages(&timeline, &format!("{build} exited with code 0"));
    assert_eq!(exits.len(), 1, "{exits:#?}");
    assert!(exits[0].contains("4 lines"), "{}", exits[0]);
    assert_eq!(job_messages(&timeline, "Background job update").len(), 3);
    assert_eq!((first.runs(), second.runs()), (1, 1), "the job ran once");
}

/// The messages the agent got that mention `needle` (wakes: as prompts or steers).
fn job_messages(timeline: &Timeline, needle: &str) -> Vec<String> {
    timeline
        .user_texts()
        .into_iter()
        .filter(|t| t.starts_with("[controlroom] Background job update") && t.contains(needle))
        .collect()
}
