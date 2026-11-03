//! Shared helpers: test servers on 127.0.0.1, an HTTP client, cookies, and a WebSocket UI
//! client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use controlroom_web::{Assets, Config, Env, Server, StartOptions, start};
use futures::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, SET_COOKIE};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::mock_idp::MockIdp;

pub const PUBLIC_HOST: &str = "controlroom.example.com";
pub const PUBLIC_ORIGIN: &str = "https://controlroom.example.com";
pub const DEV_SESSION: &str = "controlroom_session";
pub const OIDC_SESSION: &str = "__Host-controlroom_session";
/// How long a test waits for anything.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The UI files the test servers serve.
pub fn test_assets() -> Assets {
    let files: HashMap<String, Vec<u8>> = [
        ("index.html", "<!doctype html><title>controlroom</title>"),
        ("sw.js", "self.addEventListener('fetch', () => {});"),
        ("manifest.webmanifest", "{\"name\":\"controlroom\"}"),
        ("assets/index-3f2a.js", "console.log(1);"),
    ]
    .into_iter()
    .map(|(path, body)| (path.to_owned(), body.as_bytes().to_vec()))
    .collect();
    Assets::Memory(Arc::new(files))
}

/// A running test server and its state directory.
pub struct TestServer {
    pub server: Server,
    pub dir: Arc<tempfile::TempDir>,
    /// The Host header to send.
    pub host: String,
    /// The Origin a browser on the app would send.
    pub origin: String,
}

impl TestServer {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.local_addr().unwrap())
    }

    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        client().get(self.url(path)).header("host", &self.host)
    }

    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        client().post(self.url(path)).header("host", &self.host)
    }

    /// Stops the server, keeping the state directory for a restart.
    pub async fn stop(self) -> Arc<tempfile::TempDir> {
        self.server.shutdown().await.unwrap();
        self.dir
    }
}

/// An HTTP client that never follows redirects.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// A fresh state directory with a master key.
pub fn state_dir() -> Arc<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("master-key"), [42u8; 32]).unwrap();
    Arc::new(dir)
}

/// The TOML lines every test server shares.
pub fn base_toml(dir: &tempfile::TempDir) -> String {
    format!(
        "listen = \"127.0.0.1:0\"\nstate_dir = {:?}\nmaster_key_file = {:?}\n",
        dir.path().join("state"),
        dir.path().join("master-key"),
    )
}

pub async fn start_toml(toml: &str) -> Server {
    let settings = Config::from_toml(toml)
        .unwrap()
        .resolve(&Env::default())
        .unwrap();
    start(
        settings,
        StartOptions {
            assets: test_assets(),
        },
    )
    .await
    .unwrap()
}

/// A dev-token server; `extra` is appended to the TOML.
pub async fn dev_server_with(dir: Arc<tempfile::TempDir>, extra: &str) -> TestServer {
    let toml = format!("{}{extra}\n[auth]\nmode = \"dev-token\"\n", base_toml(&dir));
    let server = start_toml(&toml).await;
    let host = server.local_addr().unwrap().to_string();
    let origin = server.public_origin().to_owned();
    TestServer {
        server,
        dir,
        host,
        origin,
    }
}

pub async fn dev_server() -> TestServer {
    dev_server_with(state_dir(), "").await
}

/// An OIDC server for `idp` with the allow-list lines `allowed`.
pub async fn oidc_server_with(
    dir: Arc<tempfile::TempDir>,
    idp: &MockIdp,
    allowed: &str,
) -> TestServer {
    // The client id comes from a file too, as with identity providers that generate it.
    let id = dir.path().join("client-id");
    std::fs::write(&id, format!("{}\n", idp.client_id)).unwrap();
    let secret = dir.path().join("client-secret");
    std::fs::write(&secret, format!("{}\n", idp.client_secret)).unwrap();
    let toml = format!(
        "{}public_url = \"{PUBLIC_ORIGIN}\"\n[auth.oidc]\nissuer = \"{}\"\nclient_id_file = {id:?}\n\
         client_secret_file = {secret:?}\n{allowed}\n",
        base_toml(&dir),
        idp.issuer,
    );
    let server = start_toml(&toml).await;
    TestServer {
        server,
        dir,
        host: PUBLIC_HOST.to_owned(),
        origin: PUBLIC_ORIGIN.to_owned(),
    }
}

/// Cookie `name` from a response's Set-Cookie headers: its value and the whole line.
pub fn set_cookie(headers: &HeaderMap, name: &str) -> Option<(String, String)> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|line| {
            let (pair, _) = line.split_once(';').unwrap_or((line, ""));
            let (k, v) = pair.split_once('=')?;
            (k == name).then(|| (v.to_owned(), line.to_owned()))
        })
}

/// Signs a dev-token server in; returns the `Cookie` header value.
pub async fn dev_login(s: &TestServer) -> String {
    let url = s.server.dev_login_url().unwrap();
    let path = &url[url.find("/auth/").unwrap()..];
    let res = s.get(path).send().await.unwrap();
    assert_eq!(res.status(), 303);
    assert_eq!(res.headers()["location"], "/");
    let (value, _) = set_cookie(res.headers(), DEV_SESSION).unwrap();
    format!("{DEV_SESSION}={value}")
}

/// `GET /auth/session` with `cookie`: the status and the JSON body.
pub async fn session_info(s: &TestServer, cookie: &str) -> (u16, Value) {
    let res = s
        .get("/auth/session")
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    let status = res.status().as_u16();
    (status, res.json().await.unwrap())
}

/// Opens `/api/ws` with these headers; the HTTP status on refusal.
pub async fn ws_connect(
    s: &TestServer,
    origin: Option<&str>,
    cookie: Option<&str>,
) -> Result<Ws, u16> {
    let url = format!("ws://{}/api/ws", s.server.local_addr().unwrap());
    let mut req = url.into_client_request().unwrap();
    let headers = req.headers_mut();
    headers.insert("host", s.host.parse().unwrap());
    if let Some(origin) = origin {
        headers.insert("origin", origin.parse().unwrap());
    }
    if let Some(cookie) = cookie {
        headers.insert("cookie", cookie.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((stream, _)) => Ok(Ws { stream, next_id: 0 }),
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status().as_u16()),
        Err(e) => panic!("WebSocket connect failed: {e}"),
    }
}

/// A browser tab's WebSocket.
pub struct Ws {
    stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    next_id: i64,
}

impl Ws {
    pub async fn send(&mut self, message: Message) {
        self.stream.send(message).await.unwrap();
    }

    /// Sends without expecting success (the server may reset the connection mid-send).
    pub async fn send_unchecked(&mut self, message: Message) {
        let _ = self.stream.send(message).await;
    }

    /// Calls `method`; returns the whole response, skipping notifications.
    pub async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(Message::text(frame.to_string())).await;
        loop {
            let message = tokio::time::timeout(TIMEOUT, self.stream.next())
                .await
                .expect("a response in time")
                .expect("the socket stays open")
                .unwrap();
            let Message::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(text.as_str()).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }

    /// Calls `method` and expects success.
    pub async fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params).await;
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    /// `hello` with a device, then `devices.register`.
    pub async fn hello(&mut self, device: &str) -> Value {
        let hello = self
            .ok(
                "hello",
                json!({"protocol_version": {"major": 1, "minor": 0}, "device_id": device}),
            )
            .await;
        self.ok(
            "devices.register",
            json!({"device_id": device, "label": format!("{device} browser")}),
        )
        .await;
        hello
    }

    /// Waits for the server's close frame; returns its code.
    pub async fn closed(&mut self) -> u16 {
        loop {
            let message = tokio::time::timeout(TIMEOUT, self.stream.next())
                .await
                .expect("the server closes the socket in time");
            match message {
                Some(Ok(Message::Close(Some(frame)))) => return frame.code.into(),
                Some(Ok(Message::Close(None))) | None | Some(Err(_)) => return 0,
                Some(Ok(_)) => {}
            }
        }
    }
}
