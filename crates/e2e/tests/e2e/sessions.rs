//! What the user sees around sessions: a sandbox approval raised on a host reaches the UI and
//! the answer flows back to the host; a second session on the same directory shows an overlap;
//! a session can't be created on an unreachable machine.

use std::io::{Read, Write};
use std::net::TcpStream;

use controlroom_controller::test_support::{op, wait_until};
use controlroom_e2e::world::{Host, Which, World};
use serde_json::{Value, json};

/// POSTs a nono approval request to the host's webhook for `workspace`, as nono would; returns
/// the decision once the user answered.
fn ask(host: &Host, workspace: &str, request: Value) -> std::thread::JoinHandle<Value> {
    let state = host.state();
    let port: u16 = std::fs::read_to_string(state.join("run/webhook.port"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let secret =
        std::fs::read_to_string(state.join(format!("workspaces/{workspace}/hook.secret"))).unwrap();
    let path = format!("/nono/v1/{workspace}/{}", secret.trim());
    std::thread::spawn(move || {
        let body = json!({"backend": "controlroom", "request": request}).to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Nono-Timeout-Secs: 60\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        serde_json::from_str(body).unwrap()
    })
}

/// A `git push` as nono reports it.
fn git_push(n: u32, branch: &str) -> Value {
    json!({"capability_type": "command", "request_id": n.to_string(), "command": "git",
           "args": ["git", "push", "origin", branch]})
}

/// The session's pending sandbox request, once there is one.
async fn pending_sandbox(world: &mut World, id: &str) -> Value {
    let summary = world
        .wait_session(id, "has a sandbox approval", |s| {
            s["requests"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["kind"] == "sandbox" && r["state"] == "pending")
        })
        .await;
    summary["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "sandbox")
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_sandbox_approval_reaches_the_ui_and_the_answer_flows_back() {
    let mut world = World::start().await;
    world.devbox.dir("proj");
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/proj")], json!({}))
        .await;
    let workspace = world.session(&id)["workspaces"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Allowed once.
    let decision = ask(&world.devbox, &workspace, git_push(1, "main"));
    let request = pending_sandbox(&mut world, &id).await;
    let summary = world.session(&id);
    assert_eq!(summary["attention"], "approval", "{summary}");
    assert_eq!(request["body"]["kind"], "git_push", "{request}");
    assert_eq!(request["body"]["branch"], "main");
    assert!(request["blocking"].as_bool().unwrap());
    world
        .ui
        .ok(
            "request.respond",
            json!({"op_id": op(), "id": id, "request_id": request["id"],
                   "response": {"kind": "choice", "option_id": "allow-once"}}),
        )
        .await;
    let decision = tokio::task::spawn_blocking(move || decision.join().unwrap())
        .await
        .unwrap();
    assert_eq!(decision["decision"], "granted", "{decision}");
    world
        .wait_session(&id, "has no pending request", |s| {
            s["requests"].as_array().is_some_and(Vec::is_empty) && s["attention"].is_null()
        })
        .await;

    // Denied with a reason, which reaches nono.
    let decision = ask(&world.devbox, &workspace, git_push(2, "release"));
    let request = pending_sandbox(&mut world, &id).await;
    assert_eq!(request["body"]["branch"], "release");
    world
        .ui
        .ok(
            "request.respond",
            json!({"op_id": op(), "id": id, "request_id": request["id"],
                   "response": {"kind": "choice", "option_id": "deny", "reason": "not today"}}),
        )
        .await;
    let decision = tokio::task::spawn_blocking(move || decision.join().unwrap())
        .await
        .unwrap();
    assert_eq!(
        (decision["decision"].clone(), decision["reason"].clone()),
        (json!("denied"), json!("not today"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_session_on_the_same_directory_shows_an_overlap() {
    let mut world = World::start().await;
    let proj = world.devbox.dir("proj");
    world.buildbox.dir("proj");
    let (devbox, buildbox) = (world.devbox.id.clone(), world.buildbox.id.clone());
    let first = world
        .create_session(&[(&devbox, "~/proj")], json!({}))
        .await;
    // The same path on another machine is no overlap.
    world
        .create_session(&[(&buildbox, "~/proj")], json!({}))
        .await;
    assert_eq!(world.ui.mirror.overlaps, json!([]));

    let second = world
        .create_session(
            &[(&buildbox, "~/proj"), (&devbox, proj.to_str().unwrap())],
            json!({}),
        )
        .await;
    world
        .ui
        .wait("the overlap on devbox", |m| {
            m.overlaps.as_array().is_some_and(|o| {
                o.iter().any(|o| {
                    o["machine_id"] == devbox.as_str() && o["session_ids"] == json!([first, second])
                })
            })
        })
        .await;
    let overlaps = world.ui.mirror.overlaps.as_array().unwrap().clone();
    assert_eq!(
        overlaps.len(),
        2,
        "devbox:~/proj and buildbox:~/proj: {overlaps:#?}"
    );

    // Closing a session ends its overlaps.
    world
        .ui
        .ok("session.close", json!({"op_id": op(), "id": second}))
        .await;
    world
        .ui
        .wait("no overlaps", |m| m.overlaps == json!([]))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_on_an_unreachable_machine_is_refused() {
    let mut world = World::start().await;
    world.devbox.dir("proj");
    world.buildbox.dir("proj");
    let (devbox, buildbox) = (world.devbox.id.clone(), world.buildbox.id.clone());
    world.cut(Which::Buildbox).await;
    let params = world.create_params(&[(&devbox, "~/proj"), (&buildbox, "~/proj")], json!({}));
    let error = world
        .ui
        .err("session.create", params, "machine_unavailable")
        .await;
    assert_eq!(error["data"]["machine_id"], buildbox.as_str(), "{error}");
    assert!(world.ui.mirror.sessions.is_empty());
    // Nothing is left open on the reachable machine.
    let workspaces = world.devbox.state().join("workspaces");
    wait_until("devbox has no workspace", || {
        std::fs::read_dir(&workspaces).map_or(true, |d| d.count() == 0)
    })
    .await;

    // Once the machine is back, the same session can be created.
    world.restore(Which::Buildbox).await;
    world
        .create_session(&[(&devbox, "~/proj"), (&buildbox, "~/proj")], json!({}))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_waiting_for_input_asks_the_user_who_types_in_the_terminal() {
    use controlroom_e2e::script::{bash, say, scenario};
    let mut world = World::start().await;
    world.devbox.dir("proj");
    world.agentd.scenario(&scenario(vec![
        bash(
            "c_read",
            "devbox",
            "devbox",
            "read -r answer; echo \"got $answer\"",
        ),
        say("Thanks."),
    ]));
    let devbox = world.devbox.id.clone();
    let id = world
        .create_session(&[(&devbox, "~/proj")], json!({}))
        .await;

    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "ask me").await;
    let summary = world
        .wait_session(&id, "waits for terminal input", |s| {
            s["requests"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["kind"] == "terminal_input" && r["state"] == "pending")
        })
        .await;
    let request = &summary["requests"][0];
    assert_eq!(request["body"]["shell"], "devbox", "{request}");
    assert_eq!(request["body"]["machine"], "devbox");
    assert_eq!(summary["attention"], "approval");

    // The user types the answer in the shell's terminal; the request goes away by itself.
    world
        .devbox
        .type_into(&format!("{id}/devbox"), "yes\n")
        .await;
    world.wait_turn(&id, mark).await;
    let summary = world.session(&id);
    assert_eq!(summary["requests"], json!([]), "{summary}");
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the command's result", |t| {
            t.tool_done("c_read")
        })
        .await;
    let result = &timeline.tool_json("c_read")["result"];
    assert!(
        result["output"].as_str().unwrap().ends_with("got yes"),
        "{result}"
    );
    assert_eq!(result["waited_for_input"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_the_session_interrupts_the_remote_command() {
    use controlroom_e2e::held::Held;
    use controlroom_e2e::script::{bash, say, scenario};
    let mut world = World::start().await;
    let proj = world.buildbox.dir("proj");
    let held = Held::new(&proj, "serve");
    world.agentd.scenario(&scenario(vec![
        bash("c_env", "buildbox", "buildbox", "export KEEP=me"),
        bash(
            "c_serve",
            "buildbox",
            "buildbox",
            &held.command("echo serving", "echo stopped"),
        ),
        // Consumed by the cancelled run's next turn.
        say("Stopped."),
        bash("c_after", "buildbox", "buildbox", "echo \"[$KEEP]\""),
        say("Done."),
    ]));
    let buildbox = world.buildbox.id.clone();
    let id = world
        .create_session(&[(&buildbox, "~/proj")], json!({}))
        .await;

    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "serve").await;
    held.started().await;
    world
        .ui
        .ok("session.cancel", json!({"op_id": op(), "id": id}))
        .await;
    world.wait_turn(&id, mark).await;
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the interrupted call", |t| {
            t.tool_done("c_serve")
        })
        .await;
    let result = &timeline.tool_json("c_serve")["result"];
    assert_eq!(result["cancelled"], true, "{result}");
    assert_eq!(result["exit_code"], 130);
    // The partial output (the terminal echoes the interrupt).
    assert!(
        result["output"].as_str().unwrap().starts_with("serving"),
        "{result}"
    );
    assert_eq!(held.runs(), 1);

    // The shell survived the interrupt with its state: the next turn runs the next command.
    world.prompt(&id, "again").await;
    world
        .until(&mut timeline, "the next command", |t| {
            t.tool_done("c_after")
        })
        .await;
    assert_eq!(timeline.tool_json("c_after")["result"]["output"], "[me]");
}
