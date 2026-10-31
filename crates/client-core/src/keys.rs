//! SSH client keys: generation, OpenSSH import, public key lines and fingerprints.
//!
//! This module is pure: it never touches storage. The controller persists keys
//! sealed with [`crate::seal`] under the master key.

use std::sync::Arc;

use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
use russh::keys::ssh_key::{HashAlg, LineEnding};
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use zeroize::Zeroizing;

/// Errors from generating, importing or encoding a key.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("the private key is passphrase-protected; a passphrase is required")]
    PassphraseRequired,
    #[error("could not decrypt the private key: wrong passphrase or corrupt key")]
    WrongPassphrase,
    #[error("unsupported key type {0}: only ed25519, ecdsa and rsa keys are supported")]
    Unsupported(String),
    #[error("could not parse the private key: {0}")]
    Parse(String),
    #[error("could not encode the key: {0}")]
    Encode(String),
    #[error("system random number generator failed: {0}")]
    Random(String),
}

/// An unsealed SSH private key held in memory.
///
/// The inner key zeroizes its secret material on drop (ssh-key does this for
/// all supported key types). Cloning is cheap: the key is reference counted.
#[derive(Clone)]
pub struct SshPrivateKey {
    key: Arc<PrivateKey>,
}

impl std::fmt::Debug for SshPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshPrivateKey")
            .field("algorithm", &self.key.algorithm().as_str())
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl SshPrivateKey {
    /// Generates a new ed25519 key with the given comment, e.g.
    /// [`default_comment`]`("devbox")` = `controlroom@devbox`.
    pub fn generate_ed25519(comment: &str) -> Result<Self, KeyError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(seed.as_mut()).map_err(|e| KeyError::Random(e.to_string()))?;
        let keypair = Ed25519Keypair::from_seed(&seed);
        let key = PrivateKey::new(KeypairData::Ed25519(keypair), comment)
            .map_err(|e| KeyError::Encode(e.to_string()))?;
        Ok(Self { key: Arc::new(key) })
    }

    /// Imports a private key in OpenSSH (`BEGIN OPENSSH PRIVATE KEY`), PKCS#8,
    /// PKCS#1 (RSA) or SEC1 (EC) PEM format, as far as russh supports them.
    ///
    /// A passphrase-protected key is decrypted here, once; the result is
    /// unencrypted and meant to be sealed under the master key immediately.
    /// Security-key (`sk-*`) and DSA keys are rejected.
    pub fn import(pem: &str, passphrase: Option<&str>) -> Result<Self, KeyError> {
        let passphrase = passphrase.filter(|p| !p.is_empty());
        let key = match russh::keys::decode_secret_key(pem, passphrase) {
            Ok(key) => key,
            Err(russh::keys::Error::KeyIsEncrypted) => {
                return Err(if passphrase.is_some() {
                    KeyError::WrongPassphrase
                } else {
                    KeyError::PassphraseRequired
                });
            }
            Err(e) => {
                // Some formats report a bad passphrase as a generic decode or
                // crypto failure; disambiguate when a passphrase was given.
                if passphrase.is_some() && pem_looks_encrypted(pem) {
                    return Err(KeyError::WrongPassphrase);
                }
                return Err(KeyError::Parse(e.to_string()));
            }
        };
        Self::from_private_key(key)
    }

    /// Wraps an already-parsed key, rejecting unsupported algorithms.
    pub fn from_private_key(key: PrivateKey) -> Result<Self, KeyError> {
        if key.is_encrypted() {
            return Err(KeyError::PassphraseRequired);
        }
        match key.algorithm() {
            Algorithm::Ed25519 | Algorithm::Ecdsa { .. } | Algorithm::Rsa { .. } => {
                Ok(Self { key: Arc::new(key) })
            }
            other => Err(KeyError::Unsupported(other.as_str().to_owned())),
        }
    }

    /// Serializes the unencrypted private key in OpenSSH format, for sealing.
    pub fn to_openssh(&self) -> Result<Zeroizing<String>, KeyError> {
        self.key
            .to_openssh(LineEnding::LF)
            .map_err(|e| KeyError::Encode(e.to_string()))
    }

    /// Parses the output of [`Self::to_openssh`] (an unencrypted OpenSSH key).
    pub fn from_openssh(pem: &str) -> Result<Self, KeyError> {
        let key = PrivateKey::from_openssh(pem).map_err(|e| KeyError::Parse(e.to_string()))?;
        Self::from_private_key(key)
    }

    pub fn public_key(&self) -> &PublicKey {
        self.key.public_key()
    }

    /// The `authorized_keys` line: `ssh-ed25519 AAAA... controlroom@host`.
    pub fn public_key_line(&self) -> String {
        public_key_line(self.key.public_key())
    }

    /// `SHA256:<base64>` as printed by `ssh-keygen -l`.
    pub fn fingerprint(&self) -> String {
        fingerprint(self.key.public_key())
    }

    pub fn algorithm(&self) -> String {
        self.key.algorithm().as_str().to_owned()
    }

    pub fn comment(&self) -> &str {
        self.key.comment().as_str_lossy()
    }

    pub(crate) fn inner(&self) -> Arc<PrivateKey> {
        self.key.clone()
    }
}

/// The comment the controlroom key carries: `controlroom@<host>`.
pub fn default_comment(host: &str) -> String {
    format!("controlroom@{host}")
}

/// Formats a public key as an `authorized_keys` line (with its comment, if any).
pub fn public_key_line(key: &PublicKey) -> String {
    // Encoding a well-formed in-memory key cannot fail.
    let mut line = key.to_openssh().unwrap_or_default();
    line.truncate(line.trim_end().len());
    line
}

/// `SHA256:<unpadded base64>`, matching OpenSSH's default fingerprint format.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Returns a shell command that appends `line` to `~/.ssh/authorized_keys`, for
/// the user to run on a host before adding it.
pub fn install_command(public_key_line: &str) -> String {
    format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo {} >> ~/.ssh/authorized_keys",
        crate::shell::quote(public_key_line)
    )
}

fn pem_looks_encrypted(pem: &str) -> bool {
    if pem.contains("ENCRYPTED") {
        return true;
    }
    // OpenSSH-format keys carry the cipher name inside the base64 body.
    match PrivateKey::from_openssh(pem) {
        Ok(key) => key.is_encrypted(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_ed25519_line_and_fingerprint() {
        let key = SshPrivateKey::generate_ed25519(&default_comment("devbox")).unwrap();
        let line = key.public_key_line();
        let parts: Vec<&str> = line.split(' ').collect();
        assert_eq!(parts.len(), 3, "{line}");
        assert_eq!(parts[0], "ssh-ed25519");
        assert!(parts[1].starts_with("AAAAC3NzaC1lZDI1NTE5"), "{line}");
        assert_eq!(parts[2], "controlroom@devbox");

        let fp = key.fingerprint();
        let b64 = fp.strip_prefix("SHA256:").unwrap();
        // 32 bytes, unpadded base64.
        assert_eq!(b64.len(), 43, "{fp}");
        assert!(!b64.contains('='));
        assert_eq!(key.algorithm(), "ssh-ed25519");
        assert_eq!(key.comment(), "controlroom@devbox");
    }

    #[test]
    fn generated_keys_differ() {
        let a = SshPrivateKey::generate_ed25519("a").unwrap();
        let b = SshPrivateKey::generate_ed25519("a").unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn openssh_round_trip() {
        let key = SshPrivateKey::generate_ed25519("c").unwrap();
        let pem = key.to_openssh().unwrap();
        assert!(pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        let back = SshPrivateKey::from_openssh(&pem).unwrap();
        assert_eq!(back.public_key_line(), key.public_key_line());
        let imported = SshPrivateKey::import(&pem, None).unwrap();
        assert_eq!(imported.fingerprint(), key.fingerprint());
    }

    #[test]
    fn import_encrypted_requires_passphrase() {
        use crate::test_keys::*;
        assert!(matches!(
            SshPrivateKey::import(ED25519_ENCRYPTED, None),
            Err(KeyError::PassphraseRequired)
        ));
        assert!(matches!(
            SshPrivateKey::import(ED25519_ENCRYPTED, Some("wrong")),
            Err(KeyError::WrongPassphrase)
        ));
        let key = SshPrivateKey::import(ED25519_ENCRYPTED, Some("hunter2")).unwrap();
        assert_eq!(key.comment(), "test@example");
        assert_eq!(key.public_key_line(), ED25519_PUB);
        assert_eq!(key.fingerprint(), ED25519_FP);
        // The decrypted key re-serializes unencrypted.
        let pem = key.to_openssh().unwrap();
        assert!(SshPrivateKey::from_openssh(&pem).is_ok());
    }

    #[test]
    fn import_ecdsa_and_rsa() {
        use crate::test_keys::*;
        let ec = SshPrivateKey::import(ECDSA_P256, None).unwrap();
        assert_eq!(ec.fingerprint(), ECDSA_P256_FP);
        assert_eq!(ec.algorithm(), "ecdsa-sha2-nistp256");

        let rsa = SshPrivateKey::import(RSA_OPENSSH, None).unwrap();
        assert_eq!(rsa.fingerprint(), RSA_OPENSSH_FP);
        assert_eq!(rsa.algorithm(), "ssh-rsa");

        assert!(matches!(
            SshPrivateKey::import(RSA_PKCS1_ENCRYPTED, None),
            Err(KeyError::PassphraseRequired)
        ));
        let rsa = SshPrivateKey::import(RSA_PKCS1_ENCRYPTED, Some("pwpwpw")).unwrap();
        assert_eq!(rsa.fingerprint(), RSA_PKCS1_FP);
        // Every imported type re-serializes to unencrypted OpenSSH for sealing.
        for key in [ec, rsa] {
            let pem = key.to_openssh().unwrap();
            let back = SshPrivateKey::from_openssh(&pem).unwrap();
            assert_eq!(back.fingerprint(), key.fingerprint());
        }
    }

    #[test]
    fn import_garbage_is_parse_error() {
        assert!(matches!(
            SshPrivateKey::import("not a key", None),
            Err(KeyError::Parse(_))
        ));
    }

    #[test]
    fn install_command_quotes_line() {
        let cmd = install_command("ssh-ed25519 AAAA controlroom@host");
        assert_eq!(
            cmd,
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo 'ssh-ed25519 AAAA controlroom@host' >> ~/.ssh/authorized_keys"
        );
    }
}
