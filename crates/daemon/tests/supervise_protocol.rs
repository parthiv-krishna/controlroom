//! `controlroomd supervise`: the socket protocol's handshake, roles and errors.

mod support;

use controlroom_supervisor::{Client, ClientError, ErrorCode, PROTOCOL_VERSION};
use serde_json::json;
use support::*;

#[tokio::test]
async fn hello_is_required_and_errors_are_reported() {
    let sup = Sup::start("cat");
    let mut raw = Raw::connect(&sup.paths.socket).await;
    let msg = raw.call("status", json!({})).await;
    assert_eq!(msg["error"]["code"], "hello_required");

    let msg = raw
        .call(
            "hello",
            json!({"protocol_version": 1, "unknown_field": true}),
        )
        .await;
    let hello = &msg["result"];
    assert_eq!(hello["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(hello["id"], "s1");
    assert_eq!(hello["kind"], "agent");
    assert_eq!(hello["child_pid"], sup.info.child_pid);
    assert_eq!(hello["writer"], false);

    let msg = raw.call("bogus", json!({})).await;
    assert_eq!(msg["error"]["code"], "unknown_method");
    raw.send_line("not json").await;
    let msg = raw.message().await;
    assert_eq!(msg["error"]["code"], "parse_error");
    assert_eq!(msg["id"], serde_json::Value::Null);
    let msg = raw.call("read", json!({"from_seq": "x"})).await;
    assert_eq!(msg["error"]["code"], "invalid_params");

    let msg = raw
        .call("hello", json!({"protocol_version": 1, "writer": true}))
        .await;
    assert_eq!(msg["result"]["writer"], true);
    let msg = raw.call("write", json!({"line": "a\nb"})).await;
    assert_eq!(msg["error"]["code"], "invalid_params");
    let msg = raw.call("write", json!({})).await;
    assert_eq!(msg["error"]["code"], "invalid_params");
    let msg = raw.call("write", json!({"line": "ok"})).await;
    assert_eq!(msg["result"]["seq"], 2);
}

#[tokio::test]
async fn one_writer_at_a_time() {
    let sup = Sup::start("cat");
    let first = sup.connect(true).await;
    assert!(first.hello().writer);
    first.write("one").await.unwrap();

    match Client::connect(&sup.paths.socket, true).await {
        Err(ClientError::Remote(body)) => assert_eq!(body.code, ErrorCode::WriterBusy),
        other => panic!("second writer: {:?}", other.map(|c| c.hello().clone())),
    }
    let reader = sup.connect(false).await;
    assert!(!reader.hello().writer);
    assert!(reader.status().await.unwrap().writer_attached);
    // Readers may not write or terminate, but may read, sync and barrier.
    for result in [
        reader.write("x").await.map(|_| ()),
        reader.terminate(None).await.map(|_| ()),
    ] {
        match result {
            Err(ClientError::Remote(body)) => assert_eq!(body.code, ErrorCode::NotWriter),
            other => panic!("{other:?}"),
        }
    }
    reader.sync().await.unwrap();
    reader.barrier().await.unwrap();

    drop(first);
    wait_until_writer_released(&reader).await;
    let third = sup.connect(true).await;
    let mut stream = third.read(0).await.unwrap();
    third.write("two").await.unwrap();
    let records = records_until(&mut stream, is_out("two")).await;
    assert!(records.iter().any(is_out("one")));
}

async fn wait_until_writer_released(client: &Client) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while client.status().await.unwrap().writer_attached {
        assert!(
            tokio::time::Instant::now() < deadline,
            "writer not released"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_new_read_replaces_the_stream() {
    let sup = Sup::start("cat");
    let client = sup.connect(true).await;
    let mut old = client.read(0).await.unwrap();
    let mut new = client.read(1).await.unwrap();
    // The old stream ends (after any records that arrived before the new read).
    while next(&mut old).await.is_some() {}
    client.write("x").await.unwrap();
    let records = records_until(&mut new, is_out("x")).await;
    assert_eq!(records[0].seq, 1);
}
