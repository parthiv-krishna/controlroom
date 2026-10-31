//! SSH connection tests against the in-process russh server.
#![cfg(unix)]

mod support;

use std::time::Duration;

use controlroom_client_core::SshPrivateKey;
use controlroom_client_core::ssh::{
    ConnectOptions, ExitStatus, HostKey, SshError, SshErrorKind, SshTarget, connect, fetch_host_key,
};
use russh::keys::PrivateKey;
use support::TestServer;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn opts() -> ConnectOptions {
    ConnectOptions {
        connect_timeout: Duration::from_secs(10),
        ..Default::default()
    }
}

async fn setup() -> (TestServer, SshPrivateKey) {
    let key = SshPrivateKey::generate_ed25519("controlroom@devbox").unwrap();
    let server = TestServer::start(vec![key.public_key().clone()]).await;
    (server, key)
}

#[tokio::test]
async fn first_contact_returns_host_key_without_session() {
    let (server, key) = setup().await;
    let err = connect(&server.target(), &key, None, &opts())
        .await
        .unwrap_err();
    let SshError::UnknownHostKey { host_key } = &err else {
        panic!("expected UnknownHostKey, got {err:?}");
    };
    assert_eq!(host_key, &server.host_key);
    assert!(host_key.fingerprint().starts_with("SHA256:"));
    assert_eq!(err.kind(), SshErrorKind::UnknownHostKey);
    assert!(!err.kind().is_retryable());

    let fetched = fetch_host_key(&server.target(), &opts()).await.unwrap();
    assert_eq!(fetched, server.host_key);

    // After the user confirms, the caller pins the key and connects.
    let pinned = HostKey::parse(&host_key.to_openssh()).unwrap();
    let conn = connect(&server.target(), &key, Some(&pinned), &opts())
        .await
        .unwrap();
    assert_eq!(conn.host_key(), &server.host_key);
}

#[tokio::test]
async fn changed_host_key_is_a_hard_error() {
    let (server, key) = setup().await;
    let pinned = server.host_key.clone();
    let new = server.rotate_host_key();
    let err = connect(&server.target(), &key, Some(&pinned), &opts())
        .await
        .unwrap_err();
    match &err {
        SshError::HostKeyMismatch { expected, actual } => {
            assert_eq!(expected, &pinned);
            assert_eq!(actual, &new);
        }
        other => panic!("expected HostKeyMismatch, got {other:?}"),
    }
    assert_eq!(err.kind(), SshErrorKind::HostKeyMismatch);
    assert!(!err.kind().is_retryable());
    assert!(err.to_string().contains("HOST KEY MISMATCH"), "{err}");
}

#[tokio::test]
async fn pinned_algorithm_selects_the_pinned_host_key() {
    // The server has an ed25519 and an ECDSA host key. Without algorithm
    // pinning the client would negotiate ed25519 and report a false mismatch
    // against an ECDSA pin.
    let (server, key) = setup().await;
    let ecdsa = PrivateKey::from_openssh(support::test_keys::ECDSA_P256).unwrap();
    let ecdsa_pin = server.add_host_key(ecdsa);
    assert_eq!(ecdsa_pin.algorithm(), "ecdsa-sha2-nistp256");

    // First contact presents the client's preferred type.
    let fetched = fetch_host_key(&server.target(), &opts()).await.unwrap();
    assert_eq!(fetched, server.host_key);

    for pin in [&ecdsa_pin, &server.host_key] {
        let conn = connect(&server.target(), &key, Some(pin), &opts())
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", pin.algorithm()));
        assert_eq!(conn.host_key(), pin);
        assert_eq!(conn.run("echo ok", b"").await.unwrap().stdout, b"ok\n");
    }
}

#[tokio::test]
async fn auth_failure_is_typed() {
    let (server, key) = setup().await;
    let stranger = SshPrivateKey::generate_ed25519("").unwrap();
    let err = connect(&server.target(), &stranger, Some(&server.host_key), &opts())
        .await
        .unwrap_err();
    assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
    assert!(!err.kind().is_retryable());

    // Right key, wrong user.
    let target = SshTarget::new("127.0.0.1", server.addr.port(), "bob");
    let err = connect(&target, &key, Some(&server.host_key), &opts())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), SshErrorKind::AuthFailed, "{err:?}");
}

#[tokio::test]
async fn rsa_and_ecdsa_keys_authenticate() {
    // Imported keys of other types work too (RSA uses rsa-sha2-*).
    for pem in [
        support::test_keys::ECDSA_P256,
        support::test_keys::RSA_OPENSSH,
    ] {
        let key = SshPrivateKey::import(pem, None).unwrap();
        let server = TestServer::start(vec![key.public_key().clone()]).await;
        let conn = connect(&server.target(), &key, Some(&server.host_key), &opts())
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", key.algorithm()));
        let out = conn.run("echo ok", b"").await.unwrap();
        assert_eq!(out.stdout, b"ok\n");
    }
}

#[tokio::test]
async fn ipv6_hosts_connect_with_or_without_brackets() {
    let key = SshPrivateKey::generate_ed25519("").unwrap();
    let server = match TestServer::start_ipv6(vec![key.public_key().clone()]).await {
        Ok(server) => server,
        Err(e) => {
            eprintln!("skipping: no IPv6 loopback ({e})");
            return;
        }
    };
    for host in ["[::1]", "::1"] {
        let target = SshTarget::new(host, server.addr.port(), support::USER);
        let conn = connect(&target, &key, Some(&server.host_key), &opts())
            .await
            .unwrap_or_else(|e| panic!("{host}: {e}"));
        assert_eq!(conn.run("echo ok", b"").await.unwrap().stdout, b"ok\n");
    }
}

#[tokio::test]
async fn exec_output_stderr_and_exit_status() {
    let (server, key) = setup().await;
    let conn = connect(&server.target(), &key, Some(&server.host_key), &opts())
        .await
        .unwrap();
    let out = conn
        .run("echo hello; echo oops >&2; exit 3", b"")
        .await
        .unwrap();
    assert_eq!(out.stdout, b"hello\n");
    assert_eq!(out.stderr, b"oops\n");
    assert_eq!(out.status, ExitStatus::Code(3));
    assert!(!out.status.success());

    let out = conn.run("cat", b"stdin data").await.unwrap();
    assert_eq!(out.stdout, b"stdin data");
    assert!(out.status.success());

    let out = conn.run("printf %s \"$HOME\"", b"").await.unwrap();
    assert_eq!(out.stdout_str(), server.home_path().to_str().unwrap());
}

#[tokio::test]
async fn exec_channels_are_multiplexed_duplex_streams() {
    let (server, key) = setup().await;
    let conn = connect(&server.target(), &key, Some(&server.host_key), &opts())
        .await
        .unwrap();

    // Two line-echo channels open at once on one connection.
    let mut a = conn
        .exec("while read l; do echo \"a:$l\"; done")
        .await
        .unwrap();
    let mut b = conn
        .exec("while read l; do echo \"b:$l\"; done")
        .await
        .unwrap();
    for i in 0..3 {
        a.write_all(format!("{i}\n").as_bytes()).await.unwrap();
        b.write_all(format!("{i}\n").as_bytes()).await.unwrap();
        a.flush().await.unwrap();
        b.flush().await.unwrap();
    }
    let (ra, mut wa, mut sa) = a.into_parts();
    let mut ra = BufReader::new(ra);
    let mut line = String::new();
    for i in 0..3 {
        line.clear();
        ra.read_line(&mut line).await.unwrap();
        assert_eq!(line, format!("a:{i}\n"));
    }
    let mut out = String::new();
    b.shutdown().await.unwrap();
    b.read_to_string(&mut out).await.unwrap();
    assert_eq!(out, "b:0\nb:1\nb:2\n");
    assert_eq!(b.exit_status().await, ExitStatus::Code(0));

    wa.shutdown().await.unwrap();
    let mut rest = String::new();
    ra.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "");
    assert_eq!(sa.wait().await, ExitStatus::Code(0));

    // A large payload round-trips through cat with backpressure intact.
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let out = conn.run("cat", &big).await.unwrap();
    assert!(out.stdout == big);
}

#[tokio::test]
async fn direct_tcpip_reaches_remote_loopback() {
    let (server, key) = setup().await;
    let conn = connect(&server.target(), &key, Some(&server.host_key), &opts())
        .await
        .unwrap();
    // A one-shot uppercasing "dev server" on the remote's loopback.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let dev_server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).await.unwrap();
        s.write_all(&buf.to_ascii_uppercase()).await.unwrap();
    });
    let mut stream = conn.open_direct_tcpip("127.0.0.1", port).await.unwrap();
    stream.write_all(b"hello").await.unwrap();
    let mut buf = [0u8; 5];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"HELLO");
    dev_server.await.unwrap();

    // Nothing listening (a bound, non-listening socket holds the port): the
    // server rejects the channel.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(([127, 0, 0, 1], 0).into()).unwrap();
    let closed_port = socket.local_addr().unwrap().port();
    assert!(
        conn.open_direct_tcpip("127.0.0.1", closed_port)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn closed_resolves_when_server_disconnects() {
    let (server, key) = setup().await;
    let conn = connect(&server.target(), &key, Some(&server.host_key), &opts())
        .await
        .unwrap();
    assert!(!conn.is_closed());
    server.disconnect_all().await;
    let reason = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .expect("closed() should resolve");
    assert!(!reason.is_empty());
    assert!(conn.is_closed());
    let err = conn.run("true", b"").await.unwrap_err();
    assert_eq!(err.kind(), SshErrorKind::Disconnected, "{err:?}");
    assert!(err.kind().is_retryable());
}

#[tokio::test]
async fn unreachable_and_timeout() {
    let key = SshPrivateKey::generate_ed25519("").unwrap();
    // A bound but not listening socket holds the port (so no parallel test
    // can take it) and refuses connections.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(([127, 0, 0, 1], 0).into()).unwrap();
    let port = socket.local_addr().unwrap().port();
    let err = connect(
        &SshTarget::new("127.0.0.1", port, "alice"),
        &key,
        None,
        &opts(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), SshErrorKind::Unreachable, "{err:?}");
    assert!(err.kind().is_retryable());
    assert!(
        err.to_string().contains(&format!("127.0.0.1:{port}")),
        "{err}"
    );

    // A listener that never speaks SSH: timeout.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    let hold = tokio::spawn(async move {
        let mut held = vec![];
        while let Ok((s, _)) = silent.accept().await {
            held.push(s);
        }
    });
    let short = ConnectOptions {
        connect_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let err = connect(
        &SshTarget::new("127.0.0.1", port, "alice"),
        &key,
        None,
        &short,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SshError::Timeout { .. }), "{err:?}");
    assert!(err.kind().is_retryable());
    hold.abort();
}
