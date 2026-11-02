//! The Host allowlist, the WebSocket Origin check and the security headers.

use super::common::*;

#[tokio::test]
async fn unknown_hosts_are_refused() {
    let s = dev_server().await;
    let port = s.server.local_addr().unwrap().port();
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ] {
        let res = client()
            .get(s.url("/"))
            .header("host", host.as_str())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{host}");
    }
    for host in ["evil.example.com", "127.0.0.1", "localhost:1"] {
        let res = client()
            .get(s.url("/auth/session"))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 421, "{host}");
        // Refusals carry the security headers too.
        assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    }
    // Health probes may use any address.
    let res = client()
        .get(s.url("/healthz"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn websocket_upgrades_need_our_origin_and_a_login() {
    let s = dev_server().await;
    let cookie = dev_login(&s).await;
    for origin in [
        None,
        Some("https://evil.example.com"),
        Some("null"),
        Some("http://127.0.0.1:1"),
    ] {
        let refused = ws_connect(&s, origin, Some(&cookie)).await.err();
        assert_eq!(refused, Some(403), "{origin:?}");
    }
    let origin = s.origin.clone();
    assert_eq!(ws_connect(&s, Some(&origin), None).await.err(), Some(401));
    assert_eq!(
        ws_connect(&s, Some(&origin), Some("controlroom_session=forged"))
            .await
            .err(),
        Some(401)
    );
    assert!(ws_connect(&s, Some(&origin), Some(&cookie)).await.is_ok());
    s.server.shutdown().await.unwrap();
}
