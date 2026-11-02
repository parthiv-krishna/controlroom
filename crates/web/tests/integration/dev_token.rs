//! Dev-token sign-in, `auth/session`, logout.

use std::os::unix::fs::PermissionsExt;

use super::common::*;

#[tokio::test]
async fn dev_token_flow() {
    let s = dev_server().await;

    // Signed out: 401, and no login page in dev-token mode.
    let (status, body) = session_info(&s, "").await;
    assert_eq!(status, 401);
    assert_eq!(body["login_url"], serde_json::Value::Null);

    // The token is in a 0600 file and in the printed URL.
    let file = s.dir.path().join("state/dev-token");
    let token = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let url = s.server.dev_login_url().unwrap();
    assert!(url.starts_with(&format!("{}/auth/dev-token?token=", s.origin)));
    assert!(url.ends_with(token.trim()));

    // A wrong token is refused without a cookie.
    let res = s.get("/auth/dev-token?token=nope").send().await.unwrap();
    assert_eq!(res.status(), 401);
    assert!(set_cookie(res.headers(), DEV_SESSION).is_none());
    let res = s.get("/auth/dev-token").send().await.unwrap();
    assert_eq!(res.status(), 401);

    // The exchange sets an HttpOnly, SameSite=Lax cookie (not Secure: plain http on loopback).
    let res = s
        .get(&url[url.find("/auth/").unwrap()..])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 303);
    assert_eq!(res.headers()["cache-control"], "no-store");
    let (value, line) = set_cookie(res.headers(), DEV_SESSION).unwrap();
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(line.contains(attr), "{line}");
    }
    assert!(!line.contains("Secure"));
    let cookie = format!("{DEV_SESSION}={value}");

    // Signed in: the CSRF token, the mode and the user; the cookie slides.
    let res = s
        .get("/auth/session")
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(set_cookie(res.headers(), DEV_SESSION).is_some());
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["auth"], "dev_token");
    assert_eq!(body["user"]["id"], "dev");
    let csrf = body["csrf_token"].as_str().unwrap();
    assert_eq!(csrf.len(), 43);

    // Logout revokes the login.
    let res = s
        .post("/auth/logout")
        .header("cookie", &cookie)
        .header("origin", &s.origin)
        .header("x-csrf-token", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    let (_, cleared) = set_cookie(res.headers(), DEV_SESSION).unwrap();
    assert!(cleared.contains("Max-Age=0"));
    let res = s
        .get("/auth/session")
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    // The stale cookie is cleared.
    let (_, cleared) = set_cookie(res.headers(), DEV_SESSION).unwrap();
    assert!(cleared.contains("Max-Age=0"));
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn csrf_is_required_on_posts() {
    let s = dev_server().await;
    let cookie = dev_login(&s).await;
    let csrf = session_info(&s, &cookie).await.1["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let logout = || s.post("/auth/logout").header("cookie", &cookie);

    // No session at all.
    let res = s
        .post("/auth/logout")
        .header("x-csrf-token", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    // Missing and wrong tokens.
    assert_eq!(logout().send().await.unwrap().status(), 403);
    let res = logout().header("x-csrf-token", "x").send().await.unwrap();
    assert_eq!(res.status(), 403);
    // A foreign Origin, even with the token.
    let res = logout()
        .header("x-csrf-token", &csrf)
        .header("origin", "https://evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
    // Still signed in after all that.
    assert_eq!(session_info(&s, &cookie).await.0, 200);
    // Any other POST is guarded too.
    let res = s
        .post("/api/files/x")
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
    let res = logout().header("x-csrf-token", &csrf).send().await.unwrap();
    assert_eq!(res.status(), 204);
    s.server.shutdown().await.unwrap();
}
