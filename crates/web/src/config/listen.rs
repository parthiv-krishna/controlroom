//! Where the server listens: a TCP address or a unix socket.

use std::net::SocketAddr;
use std::path::PathBuf;

use super::ConfigError;

/// The listen address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    /// `HOST:PORT` with an IP address.
    Tcp(SocketAddr),
    /// `unix:/path/to/socket`, for a reverse proxy on the same host.
    Unix(PathBuf),
}

impl Listen {
    /// Parses `HOST:PORT` or `unix:/path`.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        if let Some(path) = value.strip_prefix("unix:") {
            if !path.starts_with('/') {
                return Err(ConfigError::Invalid(format!(
                    "listen: the unix socket path must be absolute, got {path:?}"
                )));
            }
            return Ok(Self::Unix(PathBuf::from(path)));
        }
        value.parse().map(Self::Tcp).map_err(|_| {
            ConfigError::Invalid(format!(
                "listen: expected IP:PORT or unix:/path, got {value:?}"
            ))
        })
    }

    /// Whether this is a loopback TCP address.
    pub fn is_loopback_tcp(&self) -> bool {
        matches!(self, Self::Tcp(addr) if addr.ip().is_loopback())
    }
}

impl std::fmt::Display for Listen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{addr}"),
            Self::Unix(path) => write!(f, "unix:{}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_and_unix() {
        assert_eq!(
            Listen::parse("127.0.0.1:8080").unwrap(),
            Listen::Tcp("127.0.0.1:8080".parse().unwrap())
        );
        assert!(Listen::parse("[::1]:80").unwrap().is_loopback_tcp());
        assert!(!Listen::parse("0.0.0.0:80").unwrap().is_loopback_tcp());
        let unix = Listen::parse("unix:/run/controlroom/web.sock").unwrap();
        assert!(!unix.is_loopback_tcp());
        assert_eq!(unix.to_string(), "unix:/run/controlroom/web.sock");
        for bad in ["localhost:80", "unix:", "unix:relative.sock", "8080"] {
            assert!(Listen::parse(bad).is_err(), "{bad}");
        }
    }
}
