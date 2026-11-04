//! Just enough HTTP/1.1 for nono's approval webhook: one `POST` with a `Content-Length` body
//! per connection, one response, then the connection closes. Sizes are capped, so a client
//! can't make the daemon buffer much.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Longest request line plus headers.
const MAX_HEAD: usize = 16 << 10;
/// Largest body (approval requests are a few hundred bytes).
const MAX_BODY: usize = 64 << 10;

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// `POST`, `GET`, ...
    pub method: String,
    /// The request target, e.g. `/nono/v1/<session>/<secret>`.
    pub path: String,
    /// Headers, names lower-cased.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Request {
    /// The first header named `name` (lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Why a request couldn't be read; each maps to the status answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The connection ended or failed first (nothing is answered).
    Closed,
    /// Not HTTP/1.x, or a malformed head (400).
    Malformed,
    /// No `Content-Length` (chunked bodies aren't supported; 411).
    LengthRequired,
    /// Head or body over the limits (413).
    TooLarge,
}

impl Error {
    /// The status to answer with, if any.
    pub fn status(self) -> Option<u16> {
        match self {
            Error::Closed => None,
            Error::Malformed => Some(400),
            Error::LengthRequired => Some(411),
            Error::TooLarge => Some(413),
        }
    }
}

/// Reads one request.
pub async fn read_request(stream: &mut (impl AsyncRead + Unpin)) -> Result<Request, Error> {
    let mut buf = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            break end;
        }
        if buf.len() > MAX_HEAD {
            return Err(Error::TooLarge);
        }
        let mut chunk = [0u8; 2048];
        let n = stream.read(&mut chunk).await.map_err(|_| Error::Closed)?;
        if n == 0 {
            return Err(Error::Closed);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| Error::Malformed)?;
    let mut lines = head.split("\r\n");
    let mut start = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(path), Some(version)) = (start.next(), start.next(), start.next())
    else {
        return Err(Error::Malformed);
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Error::Malformed);
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Error::Malformed)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let mut request = Request {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body: Vec::new(),
    };
    if request.header("transfer-encoding").is_some() {
        return Err(Error::LengthRequired);
    }
    let length: usize = match request.header("content-length") {
        Some(value) => value.parse().map_err(|_| Error::Malformed)?,
        None if request.method == "POST" => return Err(Error::LengthRequired),
        None => 0,
    };
    if length > MAX_BODY {
        return Err(Error::TooLarge);
    }
    let mut body = buf.split_off(head_end + 4);
    if body.len() < length {
        let start = body.len();
        body.resize(length, 0);
        stream
            .read_exact(&mut body[start..])
            .await
            .map_err(|_| Error::Closed)?;
    }
    body.truncate(length);
    request.body = body;
    Ok(request)
}

/// Writes a response with a JSON body and closes the exchange (`Connection: close`).
pub async fn write_response(
    stream: &mut (impl AsyncWrite + Unpin),
    status: u16,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Resolves once the client has gone away (end of stream or an error). Extra bytes are
/// ignored.
pub async fn closed(stream: &mut (impl AsyncRead + Unpin)) {
    let mut sink = [0u8; 256];
    while let Ok(n) = stream.read(&mut sink).await {
        if n == 0 {
            return;
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(raw: &[u8]) -> Result<Request, Error> {
        let mut input = raw;
        read_request(&mut input).await
    }

    #[tokio::test]
    async fn reads_a_post_with_its_body() {
        let r = parse(b"POST /nono/v1/x/y HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Nono-Timeout-Secs: 8\r\nContent-Length: 4\r\n\r\n{}{}trailing")
            .await
            .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/nono/v1/x/y");
        assert_eq!(r.header("x-nono-timeout-secs"), Some("8"));
        assert_eq!(r.body, b"{}{}");
    }

    #[tokio::test]
    async fn refuses_what_it_does_not_support() {
        assert_eq!(parse(b"").await, Err(Error::Closed));
        assert_eq!(parse(b"garbage\r\n\r\n").await, Err(Error::Malformed));
        assert_eq!(
            parse(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").await,
            Err(Error::LengthRequired)
        );
        assert_eq!(
            parse(b"POST / HTTP/1.1\r\n\r\n").await,
            Err(Error::LengthRequired)
        );
        assert_eq!(
            parse(b"POST / HTTP/1.1\r\nContent-Length: 999999999\r\n\r\n").await,
            Err(Error::TooLarge)
        );
        assert_eq!(
            parse(b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort").await,
            Err(Error::Closed)
        );
        let huge = vec![b'a'; MAX_HEAD + 4096];
        assert_eq!(parse(&huge).await, Err(Error::TooLarge));
    }

    #[tokio::test]
    async fn writes_a_closing_response() {
        let mut out = Vec::new();
        write_response(&mut out, 200, br#"{"decision":"granted"}"#)
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 22\r\nConnection: close\r\n\r\n"));
        assert!(text.ends_with(r#"{"decision":"granted"}"#));
    }
}
