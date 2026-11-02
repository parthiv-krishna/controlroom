//! The UI files: types, caching, the SPA fallback, reserved paths and the security headers.

use super::common::*;

#[tokio::test]
async fn files_fallback_and_headers() {
    let s = dev_server().await;

    let res = s.get("/").send().await.unwrap();
    assert_eq!(res.status(), 200);
    let h = res.headers().clone();
    assert_eq!(h["content-type"], "text/html; charset=utf-8");
    assert_eq!(h["cache-control"], "no-cache");
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("script-src 'self';"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(
        csp.contains(&format!("connect-src 'self' ws://{}", s.host)),
        "{csp}"
    );
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["referrer-policy"], "no-referrer");
    // No HSTS on plain http.
    assert!(!h.contains_key("strict-transport-security"));
    assert!(
        res.text()
            .await
            .unwrap()
            .contains("<title>controlroom</title>")
    );

    let res = s.get("/assets/index-3f2a.js").send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        res.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );

    let res = s.get("/sw.js").send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(res.headers()["cache-control"], "no-cache");

    let res = s.get("/manifest.webmanifest").send().await.unwrap();
    assert_eq!(res.headers()["content-type"], "application/manifest+json");

    // HEAD works without a body.
    let res = client()
        .head(s.url("/index.html"))
        .header("host", &s.host)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.bytes().await.unwrap().is_empty());

    // SPA fallback for one segment; deeper paths and unknown files are 404s.
    let res = s.get("/settings").send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.text().await.unwrap().contains("controlroom"));
    for path in [
        "/a/b",
        "/missing.js",
        "/assets/gone.js",
        "/api/nope",
        "/auth/nope",
    ] {
        assert_eq!(s.get(path).send().await.unwrap().status(), 404, "{path}");
    }

    // Reserved for later file and terminal streams.
    for path in ["/api/files/m/home/alice/x", "/api/stream/term/1"] {
        assert_eq!(s.get(path).send().await.unwrap().status(), 501, "{path}");
    }
    s.server.shutdown().await.unwrap();
}
