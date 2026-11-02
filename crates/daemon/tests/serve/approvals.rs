//! Sandbox approvals end to end: a nono-shaped HTTP client posts approval requests to the
//! daemon's webhook; they show up as blocking `sandbox` requests and the user's answer (or
//! its absence) is the HTTP reply.
//!
//! The sessions run the fake agent without a sandbox, so the tests put the launch's webhook
//! secret in place themselves (`sessions/<id>/hook.secret`, as the launch glue does).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::thread::JoinHandle;

use serde_json::{Value, json};

use crate::harness::*;

const SECRET: &str = "5ec2e7000000000000000000000000000000000000000000000000000000c0de";

/// A nono approval-webhook client (protocol v1, unsigned).
struct Hook {
    port: u16,
}

/// What the webhook answered: the status and the JSON body.
type Reply = (u16, Value);

impl Hook {
    fn of(env: &Env) -> Hook {
        let text = std::fs::read_to_string(env.state().join("run/webhook.port")).unwrap();
        Hook {
            port: text.trim().parse().unwrap(),
        }
    }

    /// Opens a connection and sends a request; returns the stream to read the reply from.
    fn send(&self, method: &str, path: &str, body: &str, timeout_secs: u32) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nUser-Agent: nono-cli/0.80.0\r\nX-Nono-Timeout-Secs: {timeout_secs}\r\nContent-Length: {}\r\n\r\n{body}",
            self.port,
            body.len()
        )
        .unwrap();
        stream
    }

    /// Posts and waits for the reply.
    fn post(&self, path: &str, body: &Value, timeout_secs: u32) -> Reply {
        read_reply(self.send("POST", path, &body.to_string(), timeout_secs))
    }

    /// Posts from another thread (the reply waits for the user).
    fn post_async(&self, path: String, body: Value, timeout_secs: u32) -> JoinHandle<Reply> {
        let port = self.port;
        std::thread::spawn(move || Hook { port }.post(&path, &body, timeout_secs))
    }
}

fn read_reply(mut stream: TcpStream) -> Reply {
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").expect("an HTTP response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

fn path(session: &str, secret: &str) -> String {
    format!("/nono/v1/{session}/{secret}")
}

fn command(args: &[&str]) -> Value {
    json!({"backend": "controlroom", "request": {
        "capability_type": "command", "request_id": "tool-sandbox-approve-git-1",
        "command": args[0], "args": args, "caller": "session",
        "intercept_rule": "push", "reason": null, "child_pid": 7, "session_id": "nono-1"}})
}

/// A session of the fake agent whose launch has the test's webhook secret.
fn session(env: &Env, client: &Client) -> String {
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    let file = env.state().join("sessions").join(&id).join("hook.secret");
    std::fs::write(file, SECRET).unwrap();
    id
}

fn pending_sandbox_request(client: &Client, id: &str) -> Value {
    let s = client.wait_session(id, "a sandbox request", |s| {
        s["requests"]
            .as_array()
            .is_some_and(|r| r.iter().any(|r| r["kind"] == "sandbox"))
    });
    assert_eq!(s["state"], "awaiting_permission");
    assert_eq!(s["attention"], "approval");
    s["requests"][0].clone()
}

fn no_requests(client: &Client, id: &str) -> Value {
    client.wait_session(id, "no pending request", |s| {
        s["state"] == "idle" && s["requests"].as_array().is_some_and(Vec::is_empty)
    })
}

/// The request's timeline item, from a fresh subscription.
fn timeline_request(client: &Client, id: &str, request_id: &Value) -> Value {
    let (_, ops) = client.session_ops(id, 0);
    ops.into_iter()
        .rev()
        .find(|op| op["item"]["kind"] == "request" && op["item"]["request"]["id"] == *request_id)
        .map(|op| op["item"]["request"].clone())
        .expect("a request item")
}

#[test]
fn a_git_push_approval_round_trip() {
    let env = Env::with(texts(&["Hello."]));
    let fresh = Client::connect(&env.socket());
    let hello = fresh.hello_debug();
    assert!(
        hello["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("sandbox_approvals")),
        "{hello}"
    );
    let client = env.client();
    client.watch_machine();
    let id = session(&env, &client);
    let hook = Hook::of(&env);

    // Allowed once, by a device.
    let reply = hook.post_async(
        path(&id, SECRET),
        command(&["git", "push", "origin", "main"]),
        30,
    );
    let request = pending_sandbox_request(&client, &id);
    assert_eq!(request["title"], "Run `git push origin main`?");
    assert_eq!(request["blocking"], true);
    assert_eq!(
        request["body"],
        json!({"kind": "git_push", "command": "git push origin main", "remote": "origin",
               "branch": "main", "force": false})
    );
    let options: Vec<&str> = request["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["allow-once", "deny"]);
    let device = json!({"device_id": "laptop-1", "label": "Laptop"});
    let answered = client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "allow-once"}, "by": device}),
    );
    assert_eq!(answered["request"]["answer"]["by"], device);
    assert_eq!(reply.join().unwrap(), (200, json!({"decision": "granted"})));
    no_requests(&client, &id);
    let item = timeline_request(&client, &id, &request["id"]);
    assert_eq!(item["state"], "answered");
    assert_eq!(item["answer"]["by"], device);

    // Denied with a reason; a force push says so.
    let reply = hook.post_async(
        path(&id, SECRET),
        command(&["git", "push", "--force", "origin", "main"]),
        30,
    );
    let request = pending_sandbox_request(&client, &id);
    assert_eq!(request["body"]["force"], true);
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "deny", "reason": "not on main"}}),
    );
    assert_eq!(
        reply.join().unwrap(),
        (200, json!({"decision": "denied", "reason": "not on main"}))
    );
    // A second answer learns the first.
    client.call_err(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "allow-once"}}),
        "already_resolved",
    );
    // Nothing of this reached the agent.
    assert!(
        env.lines_in(&id)
            .iter()
            .all(|l| l["type"] != "extension_ui_response")
    );
}

#[test]
fn bad_requests_are_refused_without_asking() {
    let env = Env::with(texts(&["Hello."]));
    let client = env.client();
    client.watch_machine();
    let id = session(&env, &client);
    let hook = Hook::of(&env);
    let push = command(&["git", "push"]);

    let wrong = SECRET.replace('5', "6");
    assert_eq!(hook.post(&path(&id, &wrong), &push, 30).0, 403);
    let stranger = "01HZY3K6W8Q9X2V4N7B5C1D3E6";
    assert_eq!(hook.post(&path(stranger, SECRET), &push, 30).0, 403);
    assert_eq!(hook.post("/nono/v1/not-a-session/x", &push, 30).0, 404);
    let get = read_reply(hook.send("GET", &path(&id, SECRET), "", 30));
    assert_eq!(get.0, 405);
    let malformed = read_reply(hook.send("POST", &path(&id, SECRET), "{not json", 30));
    assert_eq!(malformed.0, 400);
    // A request type controlroom can't show is denied at once.
    let (status, reply) = hook.post(
        &path(&id, SECRET),
        &json!({"backend": "controlroom", "request": {"capability_type": "teleport", "request_id": "1"}}),
        30,
    );
    assert_eq!(status, 200);
    assert_eq!(reply["decision"], "denied");
    // None of them reached the user.
    let s = no_requests(&client, &id);
    assert_eq!(s["attention"], Value::Null);
}

#[test]
fn unanswered_or_abandoned_approvals_expire() {
    let env = Env::with(texts(&["Hello."]));
    let client = env.client();
    client.watch_machine();
    let id = session(&env, &client);
    let hook = Hook::of(&env);

    // nono gives up at once: the reply is a denial and the request expires.
    let (status, reply) = hook.post(&path(&id, SECRET), &command(&["git", "push"]), 0);
    assert_eq!(status, 200);
    assert_eq!(reply["decision"], "denied");
    no_requests(&client, &id);

    // nono hangs up while the user hasn't answered: the request expires too.
    let stream = hook.send(
        "POST",
        &path(&id, SECRET),
        &command(&["git", "push", "origin", "dev"]).to_string(),
        30,
    );
    let request = pending_sandbox_request(&client, &id);
    stream.shutdown(Shutdown::Both).unwrap();
    no_requests(&client, &id);
    assert_eq!(
        timeline_request(&client, &id, &request["id"])["state"],
        "expired"
    );
    let late = client.call_err(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "allow-once"}}),
        "already_resolved",
    );
    assert_eq!(late["data"]["request"]["state"], "expired");
}

#[test]
fn a_daemon_restart_expires_waiting_approvals_and_keeps_the_port() {
    let mut env = Env::with(texts(&["Hello."]));
    let client = env.client();
    client.watch_machine();
    let id = session(&env, &client);
    let hook = Hook::of(&env);
    let reply = hook.post_async(path(&id, SECRET), command(&["git", "push"]), 30);
    let request = pending_sandbox_request(&client, &id);

    env.kill_daemon();
    client.wait_closed();
    // The HTTP request died with the daemon (nono treats that as a denial).
    let died = reply.join();
    assert!(died.is_err() || died.is_ok_and(|(status, _)| status != 200));
    env.start();
    let client = env.client();
    client.watch_machine();
    assert_eq!(
        Hook::of(&env).port,
        hook.port,
        "the port survives a restart"
    );
    no_requests(&client, &id);
    assert_eq!(
        timeline_request(&client, &id, &request["id"])["state"],
        "expired"
    );

    // The running agent's launch secret still works against the new daemon.
    let reply = hook.post_async(path(&id, SECRET), command(&["git", "push"]), 30);
    let request = pending_sandbox_request(&client, &id);
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "allow-once"}}),
    );
    assert_eq!(reply.join().unwrap(), (200, json!({"decision": "granted"})));
}
