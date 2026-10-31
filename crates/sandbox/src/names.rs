//! Validated identifiers that end up in file paths, profile keys or helper argv.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// Why an identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// Not a ULID.
    #[error("invalid session id {0:?}: expected a ULID")]
    SessionId(String),
    /// Empty, too long, or contains characters outside `[A-Za-z0-9._+-]`.
    #[error("invalid name {0:?}: expected 1-64 characters from [A-Za-z0-9._+-]")]
    Name(String),
    /// Not a plain lowercase DNS hostname.
    #[error("invalid domain {0:?}: expected a hostname without scheme, port, path or wildcard")]
    Domain(String),
}

/// A controlroom session id (a ULID string). Safe to use as a path component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Parses a ULID in canonical (uppercase) form.
    pub fn new(s: &str) -> Result<Self, NameError> {
        match ulid::Ulid::from_string(s) {
            Ok(u) if u.to_string() == s => Ok(Self(s.to_owned())),
            _ => Err(NameError::SessionId(s.to_owned())),
        }
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A short identifier for providers, credential routes and mediated commands.
///
/// Restricted to `[A-Za-z0-9._+-]` (nono's command-policy identifier alphabet), at most 64
/// characters, and never `.`/`..`, so it is also safe as a path component and argv element.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Name(String);

impl Name {
    /// Validates `s`.
    pub fn new(s: &str) -> Result<Self, NameError> {
        let ok = !s.is_empty()
            && s.len() <= 64
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(NameError::Name(s.to_owned()))
        }
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A hostname allowed through the nono proxy (`api.example.com`).
///
/// Lowercase LDH labels only: no scheme, port, path or `*` wildcard, so an allowlist entry
/// grants exactly one host. IPv4 literals and `localhost` are rejected so the proxy never
/// becomes a path to loopback services.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Domain(String);

impl Domain {
    /// Validates `s` (case-insensitive; stored lowercase).
    pub fn new(s: &str) -> Result<Self, NameError> {
        let lower = s.to_ascii_lowercase();
        let label_ok = |l: &str| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        };
        let numeric_tld = lower
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()));
        let loopback = lower == "localhost" || lower.ends_with(".localhost");
        if lower.len() <= 253 && lower.split('.').all(label_ok) && !numeric_tld && !loopback {
            Ok(Self(lower))
        } else {
            Err(NameError::Domain(s.to_owned()))
        }
    }

    /// The hostname as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! string_newtype_impls {
    ($($ty:ident),*) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                $ty::new(&s).map_err(serde::de::Error::custom)
            }
        }
    )*};
}

string_newtype_impls!(SessionId, Name, Domain);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_canonical_ulids() {
        assert!(SessionId::new("01HZY3K4M5N6P7Q8R9S0T1V2W3").is_ok());
        assert!(SessionId::new("01hzy3k4m5n6p7q8r9s0t1v2w3").is_err());
        assert!(SessionId::new("../etc").is_err());
        assert!(SessionId::new("").is_err());
    }

    #[test]
    fn names() {
        for ok in ["git", "kubectl", "cr_litellm", "git.shadow1", "a+b-c"] {
            assert!(Name::new(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a b", "x\n", &"a".repeat(65)] {
            assert!(Name::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn domains() {
        assert_eq!(
            Domain::new("API.Example.com").unwrap().as_str(),
            "api.example.com"
        );
        for bad in [
            "*.example.com",
            "https://example.com",
            "example.com:443",
            "example.com/x",
            "",
            "-a.com",
            "a..com",
            "127.0.0.1",
            "localhost",
        ] {
            assert!(Domain::new(bad).is_err(), "{bad}");
        }
    }
}
