//! Which Host and Origin values the server answers to.
//!
//! They come only from the configured `public_url` (plus the loopback aliases of the bound port
//! in dev-token mode). `X-Forwarded-*` headers are never consulted.

use url::Url;

/// The public origin and the accepted Host and Origin values.
#[derive(Debug, Clone)]
pub struct OriginPolicy {
    /// `scheme://host[:port]`, e.g. `https://controlroom.example.com`.
    pub public_origin: String,
    /// Whether the public origin is https (Secure `__Host-` cookies, HSTS).
    pub secure: bool,
    /// Lowercase `host[:port]` values accepted in the `Host` header.
    hosts: Vec<String>,
    /// Exact `Origin` values accepted on WebSocket upgrades and POSTs.
    origins: Vec<String>,
}

impl OriginPolicy {
    /// The policy for a configured `public_url`.
    pub fn from_public_url(url: &Url) -> Self {
        let mut policy = Self {
            public_origin: url.origin().ascii_serialization(),
            secure: url.scheme() == "https",
            hosts: Vec::new(),
            origins: Vec::new(),
        };
        policy.allow(url.scheme(), url.host_str().unwrap_or_default(), url.port());
        policy
    }

    /// The dev-token policy for a loopback listener without a `public_url`:
    /// `http://127.0.0.1:<port>`, also reachable as `localhost` and `[::1]`.
    pub fn loopback(port: u16) -> Self {
        let mut policy = Self {
            public_origin: format!("http://127.0.0.1:{port}"),
            secure: false,
            hosts: Vec::new(),
            origins: Vec::new(),
        };
        for host in ["127.0.0.1", "localhost", "[::1]"] {
            policy.allow("http", host, Some(port));
        }
        policy
    }

    fn allow(&mut self, scheme: &str, host: &str, port: Option<u16>) {
        let host = host.to_ascii_lowercase();
        let default_port = if scheme == "https" { 443 } else { 80 };
        match port {
            Some(port) if port != default_port => {
                self.hosts.push(format!("{host}:{port}"));
                self.origins.push(format!("{scheme}://{host}:{port}"));
            }
            _ => {
                self.hosts.push(format!("{host}:{default_port}"));
                self.hosts.push(host.clone());
                self.origins.push(format!("{scheme}://{host}"));
            }
        }
    }

    /// Whether `host` (a `Host` header value) is ours.
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.hosts.contains(&host)
    }

    /// Whether `origin` (an `Origin` header value) is ours.
    pub fn origin_allowed(&self, origin: &str) -> bool {
        self.origins.iter().any(|o| o == origin)
    }

    /// The WebSocket form of the public origin (`wss://host[:port]`), for CSP `connect-src`.
    pub fn websocket_origin(&self) -> String {
        match self.public_origin.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_policy() {
        let p = OriginPolicy::from_public_url(&Url::parse("https://CR.example.com").unwrap());
        assert_eq!(p.public_origin, "https://cr.example.com");
        assert!(p.secure);
        assert!(p.host_allowed("cr.example.com"));
        assert!(p.host_allowed("CR.example.com:443"));
        assert!(!p.host_allowed("evil.example"));
        assert!(!p.host_allowed("cr.example.com:8443"));
        assert!(p.origin_allowed("https://cr.example.com"));
        assert!(!p.origin_allowed("http://cr.example.com"));
        assert!(!p.origin_allowed("https://cr.example.com.evil.example"));
        assert!(!p.origin_allowed("null"));
        assert_eq!(p.websocket_origin(), "wss://cr.example.com");
    }

    #[test]
    fn other_ports_and_loopback() {
        let p = OriginPolicy::from_public_url(&Url::parse("https://cr.example.com:8443/").unwrap());
        assert!(p.host_allowed("cr.example.com:8443"));
        assert!(!p.host_allowed("cr.example.com"));
        let l = OriginPolicy::loopback(8080);
        assert_eq!(l.public_origin, "http://127.0.0.1:8080");
        assert!(l.host_allowed("localhost:8080"));
        assert!(!l.host_allowed("localhost:8081"));
        assert!(l.origin_allowed("http://[::1]:8080"));
        assert!(!l.secure);
        assert_eq!(l.websocket_origin(), "ws://127.0.0.1:8080");
    }
}
