//! The WebSocket bridge: `hello` and the state feed through the controller, frame rules, and
//! closing on logout and device revocation.

use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use super::common::*;

const CLOSE_UNAUTHENTICATED: u16 = 4401;

async fn signed_in() -> (TestServer, String, String) {
    let s = dev_server().await;
    let cookie = dev_login(&s).await;
    let csrf = session_info(&s, &cookie).await.1["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    (s, cookie, csrf)
}

async fn open(s: &TestServer, cookie: &str) -> Ws {
    let origin = s.origin.clone();
    ws_connect(s, Some(&origin), Some(cookie)).await.unwrap()
}

#[tokio::test]
async fn hello_and_the_state_feed_round_trip() {
    let (s, cookie, _) = signed_in().await;
    let mut ws = open(&s, &cookie).await;

    // Requests before hello are refused by the controller.
    let early = ws.call("state.subscribe", json!({"since": null})).await;
    assert_eq!(early["error"]["data"]["kind"], "hello_required", "{early}");

    let hello = ws.hello("laptop").await;
    assert_eq!(hello["server_version"], controlroom_web::VERSION);
    assert_eq!(hello["auth"], "dev_token");
    assert_eq!(hello["user"]["id"], "dev");
    assert_eq!(hello["push"], serde_json::Value::Null);

    // The state feed works with zero machines.
    let state = ws.ok("state.subscribe", json!({"since": null})).await;
    assert_eq!(state["snapshot"]["machines"], json!([]));
    assert_eq!(state["snapshot"]["sessions"], json!([]));
    let devices = &state["snapshot"]["devices"];
    assert_eq!(devices[0]["device_id"], "laptop", "{devices}");
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn binary_and_oversized_frames_close_the_socket() {
    let (s, cookie, _) = signed_in().await;
    let mut ws = open(&s, &cookie).await;
    ws.send(Message::binary(vec![1, 2, 3])).await;
    assert_eq!(ws.closed().await, 1003);

    let mut ws = open(&s, &cookie).await;
    ws.hello("laptop").await;
    let huge = "x".repeat(controlroom_web::ws::MAX_MESSAGE_BYTES + 1);
    ws.send_unchecked(Message::text(huge)).await;
    let code = ws.closed().await;
    assert!(code == 1008 || code == 1009 || code == 0, "{code}");
    // The server is fine afterwards.
    let mut ws = open(&s, &cookie).await;
    ws.hello("laptop").await;
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn logout_closes_the_logins_sockets() {
    let (s, cookie, csrf) = signed_in().await;
    let other = dev_login(&s).await;
    let mut mine = open(&s, &cookie).await;
    mine.hello("laptop").await;
    let mut theirs = open(&s, &other).await;
    theirs.hello("phone").await;

    let res = s
        .post("/auth/logout")
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(mine.closed().await, CLOSE_UNAUTHENTICATED);
    // Another login stays connected.
    theirs.ok("notifications.preferences", json!({})).await;
    // And the revoked cookie can't reconnect.
    let origin = s.origin.clone();
    assert_eq!(
        ws_connect(&s, Some(&origin), Some(&cookie)).await.err(),
        Some(401)
    );
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn removing_a_device_signs_it_out() {
    let (s, laptop_cookie, _) = signed_in().await;
    let phone_cookie = dev_login(&s).await;
    let mut laptop = open(&s, &laptop_cookie).await;
    laptop.hello("laptop").await;
    let mut phone = open(&s, &phone_cookie).await;
    phone.hello("phone").await;

    laptop
        .ok("devices.remove", json!({"device_id": "phone"}))
        .await;
    assert_eq!(phone.closed().await, CLOSE_UNAUTHENTICATED);
    // The phone's login session is revoked with it.
    assert_eq!(session_info(&s, &phone_cookie).await.0, 401);
    assert_eq!(session_info(&s, &laptop_cookie).await.0, 200);
    laptop.ok("notifications.preferences", json!({})).await;
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_closes_sockets_as_going_away() {
    let (s, cookie, _) = signed_in().await;
    let mut ws = open(&s, &cookie).await;
    ws.hello("laptop").await;
    let server = s.server;
    let shutdown = tokio::spawn(server.shutdown());
    assert_eq!(ws.closed().await, 1001);
    shutdown.await.unwrap().unwrap();
}
