//! Daemon client tests against scripted fake daemons, in memory and over SSH.
#![cfg(unix)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use controlroom_client_core::SshPrivateKey;
use controlroom_client_core::daemon::{
    CallError, CallOptions, CallTimeout, ClientOptions, DaemonClient,
};
use controlroom_client_core::ssh::{ConnectOptions, connect};
use controlroom_protocol::{ErrorData, Method, RpcNotification, RpcOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use support::TestServer;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::sync::mpsc;
use ts_rs::TS;

/// The daemon end of an in-memory connection.
struct FakeDaemon {
    requests: Lines<BufReader<ReadHalf<DuplexStream>>>,
    out: WriteHalf<DuplexStream>,
}

impl FakeDaemon {
    async fn next_request(&mut self) -> Value {
        let line = self.requests.next_line().await.unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn send(&mut self, message: Value) {
        self.send_raw(&message.to_string()).await;
    }

    async fn send_raw(&mut self, line: &str) {
        self.out
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }
}

/// A client wired to a fake daemon; notifications arrive on the receiver.
fn pair(
    options: ClientOptions,
) -> (
    DaemonClient,
    FakeDaemon,
    mpsc::UnboundedReceiver<RpcNotification<Value>>,
) {
    let (client_side, daemon_side) = tokio::io::duplex(1 << 16);
    let (cr, cw) = tokio::io::split(client_side);
    let (dr, dw) = tokio::io::split(daemon_side);
    let (tx, rx) = mpsc::unbounded_channel();
    let client = DaemonClient::start(
        cr,
        cw,
        options,
        Box::new(move |n| {
            let _ = tx.send(n);
        }),
    );
    let daemon = FakeDaemon {
        requests: BufReader::new(dr).lines(),
        out: dw,
    };
    (client, daemon, rx)
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[tokio::test]
async fn calls_hooks_notifications_and_close() {
    let (client, mut daemon, mut notes) = pair(ClientOptions::default());
    let order = Arc::new(Mutex::new(Vec::<String>::new()));

    let hook_order = order.clone();
    let options = CallOptions {
        on_response: Some(Box::new(move |r| {
            hook_order.lock().unwrap().push(format!(
                "hook:{}",
                matches!(r.outcome, RpcOutcome::Result(_))
            ));
        })),
        ..Default::default()
    };
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw_with("x", json!({"a": 1}), options).await }
    });
    let req = daemon.next_request().await;
    assert_eq!(req["jsonrpc"], "2.0");
    assert_eq!(req["method"], "x");
    assert_eq!(req["params"], json!({"a": 1}));
    daemon.send(result(&req["id"], json!({"echo": 1}))).await;
    daemon
        .send(json!({"jsonrpc": "2.0", "method": "after", "params": {"n": 1}}))
        .await;

    let note = notes.recv().await.unwrap();
    // The hook ran on the reader task before the notification was routed.
    order.lock().unwrap().push(format!("note:{}", note.method));
    assert_eq!(*order.lock().unwrap(), ["hook:true", "note:after"]);
    assert_eq!(note.params, json!({"n": 1}));
    assert_eq!(call.await.unwrap().unwrap(), json!({"echo": 1}));

    // Null params are sent as an empty object.
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("fail", Value::Null).await }
    });
    let req = daemon.next_request().await;
    assert_eq!(req["params"], json!({}), "{req}");
    daemon
        .send(json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32015, "message": "busy", "data": {"kind": "busy"}}}))
        .await;
    let err = call.await.unwrap().unwrap_err();
    let CallError::Rpc(e) = err else {
        panic!("{err:?}")
    };
    assert_eq!((e.code, e.message.as_str()), (-32015, "busy"));

    // The daemon going away fails pending calls and later ones.
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("never", Value::Null).await }
    });
    daemon.next_request().await;
    drop(daemon);
    assert!(matches!(pending.await.unwrap(), Err(CallError::Closed(_))));
    assert!(!client.closed().await.is_empty());
    assert!(client.is_closed());
    assert!(matches!(
        client.call_raw("x", Value::Null).await,
        Err(CallError::Closed(_))
    ));
}

#[tokio::test]
async fn a_bad_response_body_fails_only_its_own_call() {
    let (client, mut daemon, _notes) = pair(ClientOptions::default());
    let calls: Vec<_> = ["a", "b", "c", "d"]
        .into_iter()
        .map(|m| {
            let client = client.clone();
            tokio::spawn(async move { client.call_raw(m, Value::Null).await })
        })
        .collect();
    let mut ids = std::collections::HashMap::new();
    for _ in 0..4 {
        let req = daemon.next_request().await;
        ids.insert(
            req["method"].as_str().unwrap().to_owned(),
            req["id"].clone(),
        );
    }
    // a: an error object that doesn't parse. b: a newer daemon's unknown
    // error kind. c: neither result nor error. Plus a line that isn't JSON.
    daemon
        .send(json!({"jsonrpc": "2.0", "id": ids["a"], "error": {"code": "not a number"}}))
        .await;
    daemon
        .send(json!({"jsonrpc": "2.0", "id": ids["b"], "error": {"code": -32099, "message": "?", "data": {"kind": "from_the_future"}}}))
        .await;
    daemon.send(json!({"jsonrpc": "2.0", "id": ids["c"]})).await;
    daemon.send_raw("this is not json").await;
    daemon.send(result(&ids["d"], json!("fine"))).await;

    let mut outcomes = Vec::new();
    for call in calls {
        outcomes.push(call.await.unwrap());
    }
    assert!(is_invalid_response(&outcomes[0]), "{:?}", outcomes[0]);
    let Err(CallError::Rpc(e)) = &outcomes[1] else {
        panic!("{:?}", outcomes[1])
    };
    assert_eq!((e.code, e.data.clone()), (-32099, Some(ErrorData::Unknown)));
    assert!(is_invalid_response(&outcomes[2]), "{:?}", outcomes[2]);
    assert_eq!(outcomes[3].as_ref().unwrap(), &json!("fine"));
    assert!(!client.is_closed());
}

#[derive(Serialize, Deserialize, TS)]
struct EchoParams {
    word: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, TS)]
struct EchoResult {
    word: String,
}

/// A typed method, as the protocol crate will define them.
struct Echo;

impl Method for Echo {
    const NAME: &'static str = "echo";
    type Params = EchoParams;
    type Result = EchoResult;
}

#[tokio::test]
async fn typed_calls_and_mismatched_results() {
    let (client, mut daemon, _notes) = pair(ClientOptions::default());
    let params = EchoParams { word: "hi".into() };
    let call = tokio::spawn({
        let client = client.clone();
        async move {
            let ok = client.call::<Echo>(&params).await;
            let bad = client.call::<Echo>(&params).await;
            (ok, bad)
        }
    });
    let req = daemon.next_request().await;
    assert_eq!(
        (&req["method"], &req["params"]),
        (&json!("echo"), &json!({"word": "hi"}))
    );
    daemon.send(result(&req["id"], json!({"word": "hi"}))).await;
    let req = daemon.next_request().await;
    daemon
        .send(result(&req["id"], json!({"different": true})))
        .await;
    let (ok, bad) = call.await.unwrap();
    assert_eq!(ok.unwrap(), EchoResult { word: "hi".into() });
    let err = bad.unwrap_err();
    assert!(
        matches!(&err, CallError::Rpc(e) if e.data == Some(ErrorData::InvalidResponse) && e.message.contains("echo")),
        "{err:?}"
    );
    assert!(!client.is_closed());
}

#[tokio::test]
async fn calls_time_out_and_late_answers_are_dropped() {
    let options = ClientOptions {
        call_timeout: Duration::from_millis(50),
        ..Default::default()
    };
    let (client, mut daemon, _notes) = pair(options);
    let slow = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("slow", Value::Null).await }
    });
    let req = daemon.next_request().await;
    let err = slow.await.unwrap().unwrap_err();
    assert!(
        matches!(err, CallError::Timeout(d) if d == Duration::from_millis(50)),
        "{err:?}"
    );
    // The late answer is dropped; the connection and later calls are fine.
    daemon.send(result(&req["id"], json!("late"))).await;

    let fast = tokio::spawn({
        let client = client.clone();
        async move {
            let options = CallOptions {
                timeout: CallTimeout::After(Duration::from_secs(30)),
                ..Default::default()
            };
            client.call_raw_with("fast", Value::Null, options).await
        }
    });
    let req = daemon.next_request().await;
    assert_eq!(req["method"], "fast");
    daemon.send(result(&req["id"], json!("ok"))).await;
    assert_eq!(fast.await.unwrap().unwrap(), json!("ok"));
}

#[tokio::test]
async fn oversized_lines_are_skipped_and_daemon_requests_refused() {
    let options = ClientOptions {
        max_line_bytes: 256,
        ..Default::default()
    };
    let (client, mut daemon, mut notes) = pair(options);
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw("x", Value::Null).await }
    });
    let req = daemon.next_request().await;
    daemon
        .send(json!({"jsonrpc": "2.0", "method": "huge", "params": "a".repeat(1000)}))
        .await;
    daemon
        .send(json!({"jsonrpc": "2.0", "id": "d1", "method": "please", "params": {}}))
        .await;
    daemon.send(result(&req["id"], json!(1))).await;
    assert_eq!(call.await.unwrap().unwrap(), json!(1));

    let refusal = daemon.next_request().await;
    assert_eq!(refusal["id"], "d1");
    assert_eq!(refusal["error"]["code"], -32601);

    daemon
        .send(json!({"jsonrpc": "2.0", "method": "small"}))
        .await;
    assert_eq!(notes.recv().await.unwrap().method, "small");
}

/// A daemon in shell: answers every request with `{"result": "pong"}`.
const PONG_DAEMON: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  printf '{"jsonrpc":"2.0","id":%s,"result":"pong"}\n' "$id"
done"#;

#[tokio::test]
async fn client_over_an_ssh_exec_channel() {
    let key = SshPrivateKey::generate_ed25519("").unwrap();
    let server = TestServer::start(vec![key.public_key().clone()]).await;
    let conn = connect(
        &server.target(),
        &key,
        Some(&server.host_key),
        &ConnectOptions::default(),
    )
    .await
    .unwrap();
    let exec = conn.exec(PONG_DAEMON).await.unwrap();
    let (reader, writer, mut exit) = exec.into_parts();
    let client = DaemonClient::start(reader, writer, ClientOptions::default(), Box::new(|_| {}));
    let calls: Vec<_> = (0..20)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.call_raw("ping", json!({})).await })
        })
        .collect();
    for call in calls {
        assert_eq!(call.await.unwrap().unwrap(), json!("pong"));
    }
    // Dropping the last client sends EOF; the remote loop ends.
    drop(client);
    assert!(exit.wait().await.success());
}

fn is_invalid_response(outcome: &Result<Value, CallError>) -> bool {
    matches!(outcome, Err(CallError::Rpc(e)) if e.data == Some(ErrorData::InvalidResponse))
}

#[tokio::test]
async fn hello_sends_the_controller_role_and_maps_refusals() {
    use controlroom_client_core::daemon::{HelloError, hello};
    use controlroom_protocol::ids::{ControllerId, DaemonId};

    let controller_id = ControllerId::generate();
    let other = ControllerId::generate();
    let daemon_id = DaemonId::generate();
    let (client, mut daemon, _notes) = pair(ClientOptions::default());
    let answers = [
        json!({"result": {"daemon_version": "0.1.0", "protocol_version": {"major": 1, "minor": 3},
            "daemon_id": daemon_id, "capabilities": ["jobs", "teleport"]}}),
        json!({"error": {"code": -32019, "message": "taken", "data": {"kind": "controller_conflict",
            "controller_id": other, "connected_since": 5}}}),
        json!({"error": {"code": -32001, "message": "old", "data": {"kind": "incompatible_version",
            "protocol_version": {"major": 2, "minor": 0}, "version": "9.0.0"}}}),
        json!({"result": {"daemon_version": "9.0.0", "protocol_version": {"major": 2, "minor": 0},
            "daemon_id": daemon_id, "capabilities": []}}),
    ];
    let task = tokio::spawn({
        let client = client.clone();
        async move {
            let mut out = Vec::new();
            for _ in 0..4 {
                out.push(hello(&client, "controlroom-test", controller_id).await);
            }
            out
        }
    });
    for answer in answers {
        let req = daemon.next_request().await;
        assert_eq!(req["method"], "hello");
        assert_eq!(
            req["params"]["role"],
            json!({"kind": "controller", "controller_id": controller_id})
        );
        let mut reply = answer;
        reply["jsonrpc"] = json!("2.0");
        reply["id"] = req["id"].clone();
        daemon.send(reply).await;
    }
    let out = task.await.unwrap();
    let ok = out[0].as_ref().unwrap();
    assert_eq!((ok.daemon_id, ok.capabilities.len()), (daemon_id, 2));
    assert!(
        matches!(&out[1], Err(HelloError::ControllerConflict { controller_id: c, .. }) if *c == other)
    );
    for refused in &out[2..] {
        assert!(
            matches!(refused, Err(HelloError::Incompatible { daemon_version, .. }) if daemon_version == "9.0.0"),
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn host_hello_reads_the_epoch_and_checks_the_major() {
    use controlroom_client_core::daemon::{CallTimeout, HelloError, host_hello};
    use controlroom_protocol::ids::{ControllerId, DaemonId, Epoch};

    let (client, mut daemon, _notes) = pair(ClientOptions::default());
    let (daemon_id, epoch) = (DaemonId::generate(), Epoch::generate());
    let answers = [
        json!({"result": {"host_version": "0.2.0", "protocol_version": {"major": 1, "minor": 4},
            "daemon_id": daemon_id, "epoch": epoch, "capabilities": ["shells", "pty"]}}),
        json!({"result": {"host_version": "9.0.0", "protocol_version": {"major": 2, "minor": 0},
            "daemon_id": daemon_id, "epoch": epoch, "capabilities": []}}),
        // A per-machine daemon's answer has no epoch: not a host.
        json!({"result": {"daemon_version": "0.1.0", "protocol_version": {"major": 1, "minor": 0},
            "daemon_id": daemon_id, "capabilities": []}}),
    ];
    let task = tokio::spawn({
        let client = client.clone();
        async move {
            let mut out = Vec::new();
            for _ in 0..3 {
                out.push(host_hello(&client, "controlroom-test", ControllerId::generate()).await);
            }
            out
        }
    });
    for answer in answers {
        let req = daemon.next_request().await;
        assert_eq!(
            req["params"]["protocol_version"],
            json!({"major": 1, "minor": 0})
        );
        let mut reply = answer;
        reply["jsonrpc"] = json!("2.0");
        reply["id"] = req["id"].clone();
        daemon.send(reply).await;
    }
    let out = task.await.unwrap();
    let ok = out[0].as_ref().unwrap();
    assert_eq!((ok.epoch, ok.capabilities.len()), (epoch, 2));
    assert!(
        matches!(&out[1], Err(HelloError::Incompatible { daemon_version, .. }) if daemon_version == "9.0.0")
    );
    assert!(
        matches!(&out[2], Err(HelloError::Call(e)) if e.rpc().is_some()),
        "{:?}",
        out[2]
    );

    // A call without a timeout still fails when the connection closes.
    let options = CallOptions {
        timeout: CallTimeout::Never,
        ..CallOptions::default()
    };
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_raw_with("tool.run", json!({}), options).await }
    });
    daemon.next_request().await;
    client.close();
    assert!(matches!(call.await.unwrap(), Err(CallError::Closed(_))));
}
