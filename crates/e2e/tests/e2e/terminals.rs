//! The user's own terminals: a session where the user opens a sandboxed and an unsandboxed
//! terminal on devbox and types in them; the agent's `shell_list` and `session.info` don't
//! show them, and its `bash` naming one fails without running.

use controlroom_e2e::script::{bash, say, scenario, tool};
use controlroom_e2e::world::{World, wait_file};
use serde_json::{Value, json};

/// The shells of a summary, as `(name, owner, sandbox)`.
fn shells(summary: &Value) -> Vec<(String, String, String)> {
    summary["shells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let text = |k: &str| s[k].as_str().unwrap_or_default().to_owned();
            (text("name"), text("owner"), text("sandbox"))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_users_terminals_are_out_of_the_agents_reach() {
    let mut world = World::start().await;
    let api = world.devbox.dir("src/api");
    world.agentd.scenario(&scenario(vec![
        tool("c_list", "shell_list", &[("shell.list", json!({}))]),
        tool("c_info", "info", &[("session.info", json!({}))]),
        bash(
            "c_sneak",
            "devbox",
            "you@devbox-2",
            "echo sneaked > sneaked.txt",
        ),
        bash("c_own", "devbox", "devbox", "echo own > own.txt"),
        say("Done."),
    ]));
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/src/api")], json!({}))
        .await;
    let workspace = world.session(&id)["workspaces"][0]["id"].clone();
    let open = |op: &str, sandbox: &str, confirm: bool| {
        json!({"op_id": op, "id": id, "workspace_id": workspace, "sandbox": sandbox,
               "confirm": confirm})
    };

    // An unsandboxed terminal needs the confirm.
    world
        .ui
        .err(
            "session.terminal_open",
            open("t0", "unsandboxed", false),
            "invalid_params",
        )
        .await;
    let sandboxed = world
        .ui
        .ok("session.terminal_open", open("t1", "workspace", false))
        .await;
    let unsandboxed = world
        .ui
        .ok("session.terminal_open", open("t2", "unsandboxed", true))
        .await;
    assert_eq!(
        (sandboxed["name"].clone(), unsandboxed["name"].clone()),
        (json!("you@devbox"), json!("you@devbox-2"))
    );
    let summary = world
        .wait_session(&id, "both terminals", |s| {
            s["shells"].as_array().is_some_and(|a| a.len() == 3)
        })
        .await;
    let owned = |name: &str, owner: &str, sandbox: &str| {
        (name.to_owned(), owner.to_owned(), sandbox.to_owned())
    };
    assert_eq!(
        shells(&summary),
        [
            owned("devbox", "agent", "workspace"),
            owned("you@devbox", "user", "workspace"),
            owned("you@devbox-2", "user", "unsandboxed"),
        ]
    );

    // The user types in theirs.
    world
        .devbox
        .type_into(&format!("{id}/you@devbox-2"), "echo typed > typed.txt\r")
        .await;
    wait_file(&api.join("typed.txt")).await;

    // The agent sees and reaches only its own shell.
    world.prompt(&id, "look around").await;
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the agent's own command", |t| {
            t.tool_done("c_own")
        })
        .await;
    let listed: Vec<Value> = timeline.tool_json("c_list")["shells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(listed, [json!("devbox")]);
    let info = timeline.tool_json("c_info");
    assert_eq!(info["shells"].as_array().unwrap().len(), 1, "{info}");
    assert!(timeline.tool_failed("c_sneak"));
    assert_eq!(
        timeline.tool_json("c_sneak")["data"]["kind"],
        "shell_gone",
        "{}",
        timeline.tool_json("c_sneak")
    );
    assert!(!api.join("sneaked.txt").exists());
    assert!(!timeline.tool_failed("c_own"));
    wait_file(&api.join("own.txt")).await;

    // Closing a terminal ends it; the other one stays.
    world
        .ui
        .ok(
            "session.terminal_close",
            json!({"op_id": "t3", "id": id, "name": "you@devbox"}),
        )
        .await;
    let summary = world
        .wait_session(&id, "the closed terminal gone", |s| {
            s["shells"].as_array().is_some_and(|a| a.len() == 2)
        })
        .await;
    assert_eq!(summary["shells"][1]["name"], "you@devbox-2");
}
