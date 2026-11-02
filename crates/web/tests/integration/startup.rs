//! Startup: refusals, the unix socket listener, and the binary's `--version`.

use controlroom_web::{Config, Env, StartOptions, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::common::*;

#[tokio::test]
async fn a_missing_or_bad_master_key_is_refused() {
    let dir = state_dir();
    for (name, contents) in [("missing", None), ("short", Some(&b"tiny"[..]))] {
        let path = dir.path().join(name);
        if let Some(contents) = contents {
            std::fs::write(&path, contents).unwrap();
        }
        let toml = format!(
            "listen = \"127.0.0.1:0\"\nstate_dir = {:?}\nmaster_key_file = {path:?}\n",
            dir.path().join("state")
        );
        let settings = Config::from_toml(&toml)
            .unwrap()
            .resolve(&Env::default())
            .unwrap();
        let err = start(settings, StartOptions::default())
            .await
            .err()
            .expect("refused");
        assert!(err.to_string().contains("refusing to start"), "{err}");
    }
}

#[tokio::test]
async fn serves_on_a_unix_socket_with_oidc() {
    let dir = state_dir();
    let socket = dir.path().join("web.sock");
    let secret = dir.path().join("client-secret");
    std::fs::write(&secret, "s3cret\n").unwrap();
    // The identity provider is only contacted at sign-in.
    let toml = format!(
        "listen = \"unix:{}\"\npublic_url = \"{PUBLIC_ORIGIN}\"\nstate_dir = {:?}\n\
         master_key_file = {:?}\n[auth.oidc]\nissuer = \"https://id.example.com\"\n\
         client_id = \"controlroom\"\nclient_secret_file = {secret:?}\n\
         allowed_subjects = [\"alice\"]\n",
        socket.display(),
        dir.path().join("state"),
        dir.path().join("master-key"),
    );
    let server = start_toml(&toml).await;
    assert_eq!(server.local_addr(), None);
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /auth/session HTTP/1.1\r\nHost: {PUBLIC_HOST}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(
        response.contains("\"login_url\":\"auth/login\""),
        "{response}"
    );
    server.shutdown().await.unwrap();
}

#[test]
fn version_flag() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_controlroom-web"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("controlroom-web {}", controlroom_web::VERSION)
    );
}
