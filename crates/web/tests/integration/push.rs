//! Web Push: the VAPID key in `hello` and the endpoint SSRF guard.

use controlroom_controller::VapidKey;
use data_encoding::BASE64URL_NOPAD;
use serde_json::json;

use super::common::*;

#[tokio::test]
async fn hello_carries_the_key_and_endpoints_are_validated() {
    let dir = state_dir();
    // A raw P-256 scalar (any nonzero value below the group order).
    let scalar: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
    let key_text = BASE64URL_NOPAD.encode(&scalar);
    let key_file = dir.path().join("vapid");
    std::fs::write(&key_file, format!("{key_text}\n")).unwrap();
    let expected = VapidKey::parse(&key_text).unwrap().public_key_base64url();
    let extra =
        format!("[push]\nvapid_key_file = {key_file:?}\nsubject = \"mailto:alice@example.com\"");
    let s = dev_server_with(dir, &extra).await;
    let cookie = dev_login(&s).await;
    let origin = s.origin.clone();
    let mut ws = ws_connect(&s, Some(&origin), Some(&cookie)).await.unwrap();

    let hello = ws.hello("phone").await;
    assert_eq!(hello["push"]["vapid_public_key"], expected);

    let keys = json!({"p256dh": "BPubKey", "auth": "c2VjcmV0"});
    for endpoint in [
        "http://push.example.com/send/1",
        "https://127.0.0.1/send/1",
        "https://[::1]/send/1",
        "https://169.254.169.254/latest/meta-data",
        "https://localhost/send/1",
        "https://intranet/send/1",
        "https://push.example.com:8443/send/1",
        "https://alice:pw@push.example.com/send/1",
    ] {
        let res = ws
            .call(
                "notifications.subscribe",
                json!({"device_id": "phone", "subscription": {"endpoint": endpoint, "keys": keys}}),
            )
            .await;
        assert_eq!(
            res["error"]["data"]["kind"], "invalid_params",
            "{endpoint}: {res}"
        );
    }
    let device = ws
        .ok(
            "notifications.subscribe",
            json!({"device_id": "phone", "subscription": {
                "endpoint": "https://push.example.com/send/1", "keys": keys}}),
        )
        .await;
    assert_eq!(device["push"], true);
    s.server.shutdown().await.unwrap();
}
