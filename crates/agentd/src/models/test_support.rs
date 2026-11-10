//! Test helpers.

use std::path::Path;
use std::process::Command;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Writes an executable script from a child shell: writing it from this process would race with
/// concurrent forks of other tests, which then fail to execute it with ETXTBSY.
pub(crate) fn write_executable(path: &Path, content: &str) {
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
        .arg("sh")
        .arg(content)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

/// A one-request HTTP server on loopback, answering 200 when the request (lowercased) contains
/// `accept`, e.g. `x-api-key: good-key`, and 401 otherwise, with a body quoting a key. Returns
/// its origin and a task yielding the lowercased request.
pub(crate) async fn one_request_server(accept: &'static str) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 8192];
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
        let status = if request.contains(accept) {
            "200 OK"
        } else {
            "401 Unauthorized"
        };
        let response =
            format!("HTTP/1.1 {status}\r\ncontent-length: 21\r\n\r\nkey good-key is wrong");
        stream.write_all(response.as_bytes()).await.unwrap();
        request
    });
    (origin, task)
}
