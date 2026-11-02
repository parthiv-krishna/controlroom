//! OIDC sign-in against the mock identity provider.

use std::collections::HashMap;

use serde_json::json;

use super::common::*;
use super::mock_idp::{MockIdp, user};

const LOGIN: &str = "__Host-controlroom_login";

struct Started {
    login_cookie: String,
    authorize_url: url::Url,
}

async fn start_login(s: &TestServer, return_to: &str) -> Started {
    let res = s
        .get(&format!("/auth/login?return_to={return_to}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 303, "{}", res.text().await.unwrap());
    let (login_cookie, line) = set_cookie(res.headers(), LOGIN).unwrap();
    for attr in [
        "Secure",
        "HttpOnly",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=600",
    ] {
        assert!(line.contains(attr), "{line}");
    }
    let authorize_url = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    Started {
        login_cookie,
        authorize_url,
    }
}

/// Follows the provider's authorization; returns the callback path and query.
async fn authorize(started: &Started) -> String {
    let q: HashMap<_, _> = started.authorize_url.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["redirect_uri"], format!("{PUBLIC_ORIGIN}/auth/callback"));
    let res = client()
        .get(started.authorize_url.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 302, "{}", res.text().await.unwrap());
    let back = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(back.origin().ascii_serialization(), PUBLIC_ORIGIN);
    format!("{}?{}", back.path(), back.query().unwrap())
}

async fn callback(s: &TestServer, path: &str, login_cookie: Option<&str>) -> reqwest::Response {
    let mut req = s.get(path);
    if let Some(c) = login_cookie {
        req = req.header("cookie", format!("{LOGIN}={c}"));
    }
    req.send().await.unwrap()
}

/// A full sign-in; returns the session `Cookie` header value.
async fn sign_in(s: &TestServer) -> String {
    let started = start_login(s, "/").await;
    let cb = authorize(&started).await;
    let res = callback(s, &cb, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 303, "{}", res.text().await.unwrap());
    let (session, _) = set_cookie(res.headers(), OIDC_SESSION).unwrap();
    format!("{OIDC_SESSION}={session}")
}

#[tokio::test]
async fn sign_in_by_subject_then_logout() {
    let idp = MockIdp::start(user("alice", &[], false)).await;
    let s = oidc_server_with(state_dir(), &idp, "allowed_subjects = [\"alice\"]").await;

    let (status, body) = session_info(&s, "").await;
    assert_eq!(status, 401);
    assert_eq!(body["login_url"], "auth/login");

    let started = start_login(&s, "/%23/s/m1/s1").await;
    let scope = started
        .authorize_url
        .query_pairs()
        .find(|(k, _)| k == "scope")
        .unwrap()
        .1
        .into_owned();
    assert!(scope.split(' ').any(|s| s == "openid"), "{scope}");
    let cb = authorize(&started).await;
    let res = callback(&s, &cb, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 303);
    assert_eq!(res.headers()["location"], "/#/s/m1/s1");
    let (session, line) = set_cookie(res.headers(), OIDC_SESSION).unwrap();
    for attr in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(line.contains(attr), "{line}");
    }
    assert!(
        !line.contains("Domain"),
        "__Host- cookies never set a Domain"
    );
    let (_, cleared) = set_cookie(res.headers(), LOGIN).unwrap();
    assert!(cleared.contains("Max-Age=0"));
    // HSTS over https.
    assert!(res.headers().contains_key("strict-transport-security"));

    let cookie = format!("{OIDC_SESSION}={session}");
    let (status, body) = session_info(&s, &cookie).await;
    assert_eq!(status, 200);
    assert_eq!(body["auth"], "oidc");
    assert_eq!(body["user"]["id"], "alice");
    let csrf = body["csrf_token"].as_str().unwrap().to_owned();

    // The UI hello reports the OIDC subject.
    let mut ws = ws_connect(&s, Some(PUBLIC_ORIGIN), Some(&cookie))
        .await
        .unwrap();
    let hello = ws.hello("laptop").await;
    assert_eq!(hello["auth"], "oidc");
    assert_eq!(hello["user"]["id"], "alice");

    // Replaying the callback fails: the code was single-use.
    let res = callback(&s, &cb, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 502);
    assert!(set_cookie(res.headers(), OIDC_SESSION).is_none());

    let res = s
        .post("/auth/logout")
        .header("cookie", &cookie)
        .header("origin", PUBLIC_ORIGIN)
        .header("x-csrf-token", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(ws.closed().await, 4401);
    assert_eq!(session_info(&s, &cookie).await.0, 401);
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn sign_in_by_group_from_the_id_token_or_userinfo() {
    let idp = MockIdp::start(user("bob", &["controlroom-users"], true)).await;
    let s = oidc_server_with(
        state_dir(),
        &idp,
        "allowed_groups = [\"controlroom-users\"]",
    )
    .await;
    for in_id_token in [true, false] {
        idp.set_user(user("bob", &["controlroom-users"], in_id_token));
        let started = start_login(&s, "/").await;
        assert!(
            started
                .authorize_url
                .query_pairs()
                .any(|(k, v)| { k == "scope" && v.split(' ').any(|x| x == "groups") })
        );
        let cookie = sign_in(&s).await;
        assert_eq!(session_info(&s, &cookie).await.0, 200, "{in_id_token}");
    }
    // Not in the group: denied.
    idp.set_user(user("carol", &["guests"], true));
    let started = start_login(&s, "/").await;
    let cb = authorize(&started).await;
    let res = callback(&s, &cb, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 403);
    assert!(set_cookie(res.headers(), OIDC_SESSION).is_none());
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn state_and_the_login_cookie_are_checked() {
    let idp = MockIdp::start(user("alice", &[], false)).await;
    let s = oidc_server_with(state_dir(), &idp, "allowed_subjects = [\"alice\"]").await;

    // No login cookie (another browser, or login CSRF).
    let started = start_login(&s, "/").await;
    let cb = authorize(&started).await;
    assert_eq!(callback(&s, &cb, None).await.status(), 400);
    // A forged login cookie.
    assert_eq!(callback(&s, &cb, Some("forged")).await.status(), 400);

    // A tampered state.
    let started = start_login(&s, "/").await;
    let cb = authorize(&started).await;
    let mut url = url::Url::parse(&format!("https://x{cb}")).unwrap();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if k == "state" {
                "forged".into()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
    let tampered = format!("{}?{}", url.path(), url.query().unwrap());
    let res = callback(&s, &tampered, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 400);

    // A provider error.
    let state = started
        .authorize_url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let path = format!("/auth/callback?error=access_denied&state={state}");
    let res = callback(&s, &path, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 400);

    // Open redirects are neutralized.
    let started = start_login(&s, "//evil.example.com/x").await;
    let cb = authorize(&started).await;
    let res = callback(&s, &cb, Some(&started.login_cookie)).await;
    assert_eq!(res.status(), 303);
    assert_eq!(res.headers()["location"], "/");
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_allow_list_is_rechecked_after_a_change() {
    let idp = MockIdp::start(user("alice", &["staff"], true)).await;
    let dir = state_dir();
    let s = oidc_server_with(
        dir.clone(),
        &idp,
        "allowed_subjects = [\"alice\"]\nallowed_groups = [\"staff\"]",
    )
    .await;
    let cookie = sign_in(&s).await;
    assert_eq!(session_info(&s, &cookie).await.0, 200);
    s.stop().await;

    // Still admitted through the group after alice leaves the subject list.
    let s = oidc_server_with(dir.clone(), &idp, "allowed_groups = [\"staff\"]").await;
    assert_eq!(session_info(&s, &cookie).await.0, 200);
    s.stop().await;

    // Neither: the existing session is refused, over HTTP and on the WebSocket.
    let s = oidc_server_with(dir.clone(), &idp, "allowed_subjects = [\"bob\"]").await;
    assert_eq!(session_info(&s, &cookie).await.0, 401);
    assert_eq!(
        ws_connect(&s, Some(PUBLIC_ORIGIN), Some(&cookie))
            .await
            .err(),
        Some(401)
    );
    s.stop().await;

    // Allowed again: the session (never deleted) works, but not under dev-token mode.
    let s = oidc_server_with(dir.clone(), &idp, "allowed_subjects = [\"alice\"]").await;
    let mut ws = ws_connect(&s, Some(PUBLIC_ORIGIN), Some(&cookie))
        .await
        .unwrap();
    ws.ok(
        "hello",
        json!({"protocol_version": {"major": 1, "minor": 0}}),
    )
    .await;
    drop(ws);
    s.stop().await;
    let token = cookie.split_once('=').unwrap().1;
    let dev = dev_server_with(dir, "").await;
    let dev_cookie = format!("{DEV_SESSION}={token}");
    assert_eq!(session_info(&dev, &dev_cookie).await.0, 401);
    dev.server.shutdown().await.unwrap();
}
