//! Server host keys: first-contact confirmation and pinning.

use russh::keys::{Algorithm, PublicKey};
use serde::{Deserialize, Serialize};

use crate::keys::fingerprint;

/// A server's host public key, as pinned in a machine record.
///
/// Serializes as the OpenSSH public key string without a comment
/// (`ssh-ed25519 AAAA...`), which is what the caller stores.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostKey {
    key: Box<PublicKey>,
}

/// A string that is not an OpenSSH public key.
#[derive(Debug, thiserror::Error)]
#[error("invalid host key {input:?}: {reason}")]
pub struct HostKeyParseError {
    input: String,
    reason: String,
}

impl HostKey {
    /// Pins `key`'s material; its comment is not part of the host's identity.
    pub fn from_public_key(key: &PublicKey) -> Self {
        Self {
            key: Box::new(PublicKey::new(key.key_data().clone(), "")),
        }
    }

    /// Parses `<algorithm> <base64>` (an optional trailing comment is ignored).
    pub fn parse(openssh: &str) -> Result<Self, HostKeyParseError> {
        PublicKey::from_openssh(openssh.trim())
            .map(|k| Self::from_public_key(&k))
            .map_err(|e| HostKeyParseError {
                input: openssh.to_owned(),
                reason: e.to_string(),
            })
    }

    /// `ssh-ed25519 AAAA...`
    pub fn to_openssh(&self) -> String {
        crate::keys::public_key_line(&self.key)
    }

    /// `SHA256:<base64>`, the form shown to the user for confirmation.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.key)
    }

    /// The key type, e.g. `ssh-ed25519`.
    pub fn algorithm(&self) -> String {
        self.key.algorithm().as_str().to_owned()
    }

    /// The public key itself.
    pub fn public_key(&self) -> &PublicKey {
        &self.key
    }

    /// Whether the server presented this key (comparing key material only).
    pub fn matches(&self, presented: &PublicKey) -> bool {
        self.key.key_data() == presented.key_data()
    }

    /// The host-key signature algorithms to offer when this key is pinned:
    /// the entries of `supported` (most preferred first) that this key can
    /// sign with. A host with several host keys then presents the pinned one
    /// instead of whichever type the client happens to prefer.
    pub(crate) fn preferred_algorithms(&self, supported: &[Algorithm]) -> Vec<Algorithm> {
        let pinned = self.key.algorithm();
        let same_type = |a: &Algorithm| match (a, &pinned) {
            // An RSA key signs with any rsa-sha2-* (or ssh-rsa) algorithm.
            (Algorithm::Rsa { .. }, Algorithm::Rsa { .. }) => true,
            (a, pinned) => a == pinned,
        };
        let algorithms: Vec<Algorithm> =
            supported.iter().filter(|a| same_type(a)).cloned().collect();
        if algorithms.is_empty() {
            vec![pinned]
        } else {
            algorithms
        }
    }
}

impl std::fmt::Debug for HostKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HostKey({} {})", self.algorithm(), self.fingerprint())
    }
}

impl std::fmt::Display for HostKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_openssh())
    }
}

impl TryFrom<String> for HostKey {
    type Error = HostKeyParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<HostKey> for String {
    fn from(k: HostKey) -> String {
        k.to_openssh()
    }
}

/// The outcome of checking a presented host key against the pinned one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyCheck {
    /// The presented key is the pinned one.
    Trusted,
    /// No key pinned yet: the user must confirm this key.
    Unknown(HostKey),
    /// A different key than the pinned one.
    Mismatch {
        /// The pinned key.
        expected: HostKey,
        /// The presented key.
        actual: HostKey,
    },
}

/// Checks the key a server presented against the pinned one, if any.
pub fn check_host_key(pinned: Option<&HostKey>, presented: &PublicKey) -> HostKeyCheck {
    match pinned {
        None => HostKeyCheck::Unknown(HostKey::from_public_key(presented)),
        Some(p) if p.matches(presented) => HostKeyCheck::Trusted,
        Some(p) => HostKeyCheck::Mismatch {
            expected: p.clone(),
            actual: HostKey::from_public_key(presented),
        },
    }
}

#[cfg(test)]
mod tests {
    use russh::keys::HashAlg;
    use russh::keys::ssh_key::EcdsaCurve;

    use super::*;
    use crate::SshPrivateKey;

    #[test]
    fn parse_display_and_serde_round_trip() {
        let key = SshPrivateKey::generate_ed25519("host comment").unwrap();
        let hk = HostKey::from_public_key(key.public_key());
        let s = hk.to_openssh();
        assert!(s.starts_with("ssh-ed25519 AAAA"));
        assert!(!s.contains("comment"), "{s}");
        assert_eq!(HostKey::parse(&key.public_key_line()).unwrap(), hk);
        let json = serde_json::to_string(&hk).unwrap();
        assert_eq!(json, format!("\"{s}\""));
        assert_eq!(serde_json::from_str::<HostKey>(&json).unwrap(), hk);
        assert_eq!(hk.fingerprint(), key.fingerprint());
        assert!(HostKey::parse("ssh-ed25519 notbase64!").is_err());
    }

    #[test]
    fn check_outcomes() {
        let a = SshPrivateKey::generate_ed25519("").unwrap();
        let b = SshPrivateKey::generate_ed25519("").unwrap();
        let pa = HostKey::from_public_key(a.public_key());
        assert_eq!(
            check_host_key(None, a.public_key()),
            HostKeyCheck::Unknown(pa.clone())
        );
        assert_eq!(
            check_host_key(Some(&pa), a.public_key()),
            HostKeyCheck::Trusted
        );
        assert!(matches!(
            check_host_key(Some(&pa), b.public_key()),
            HostKeyCheck::Mismatch { .. }
        ));
    }

    #[test]
    fn preferred_algorithms_follow_the_pin() {
        let supported = russh::Preferred::DEFAULT.key;
        let ed = SshPrivateKey::generate_ed25519("").unwrap();
        let ed = HostKey::from_public_key(ed.public_key());
        assert_eq!(ed.preferred_algorithms(&supported), [Algorithm::Ed25519]);

        let ec = SshPrivateKey::import(crate::test_keys::ECDSA_P256, None).unwrap();
        let ec = HostKey::from_public_key(ec.public_key());
        assert_eq!(
            ec.preferred_algorithms(&supported),
            [Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256
            }]
        );

        let rsa = SshPrivateKey::import(crate::test_keys::RSA_OPENSSH, None).unwrap();
        let rsa = HostKey::from_public_key(rsa.public_key());
        let algs = rsa.preferred_algorithms(&supported);
        assert_eq!(
            algs[0],
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            }
        );
        assert!(algs.iter().all(|a| matches!(a, Algorithm::Rsa { .. })));

        // An algorithm the client doesn't list is still offered alone.
        assert_eq!(ed.preferred_algorithms(&[]), [Algorithm::Ed25519]);
    }
}
