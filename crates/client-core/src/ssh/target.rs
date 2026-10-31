//! Where to connect: a machine record's host, port and user.

use std::fmt;

/// Where to connect: `{host, port, username}` from a machine record.
///
/// `host` is a DNS name or an IP address. IPv6 addresses may be given bare
/// (`::1`) or bracketed (`[::1]`), as users tend to copy them from URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// DNS name or IP address, possibly a bracketed IPv6 address.
    pub host: String,
    /// TCP port, usually 22.
    pub port: u16,
    /// The remote login user.
    pub username: String,
}

impl SshTarget {
    /// A target for `username@host:port`.
    pub fn new(host: impl Into<String>, port: u16, username: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
        }
    }

    /// The host as the resolver wants it: brackets around an IPv6 address removed.
    pub fn connect_host(&self) -> &str {
        let host = self.host.trim();
        host.strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
    }

    /// `host:port`, with an IPv6 host bracketed (`[::1]:22`), for messages.
    pub fn addr(&self) -> String {
        let host = self.connect_host();
        if host.contains(':') {
            format!("[{host}]:{}", self.port)
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

impl fmt::Display for SshTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.username, self.addr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_brackets() {
        let bare = SshTarget::new("::1", 22, "alice");
        let bracketed = SshTarget::new("[::1]", 22, "alice");
        for t in [&bare, &bracketed] {
            assert_eq!(t.connect_host(), "::1");
            assert_eq!(t.addr(), "[::1]:22");
            assert_eq!(t.to_string(), "alice@[::1]:22");
        }
        let name = SshTarget::new("devbox.example.com", 2222, "alice");
        assert_eq!(name.connect_host(), "devbox.example.com");
        assert_eq!(name.addr(), "devbox.example.com:2222");
        assert_eq!(SshTarget::new("10.0.0.1", 22, "a").addr(), "10.0.0.1:22");
    }
}
