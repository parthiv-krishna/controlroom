//! Sealing private key material under the master key (spec §12, §14: the required master key).
//!
//! Envelope format, version 1 (all lengths in bytes):
//!
//! ```text
//! "CRSK" (4) | version = 0x01 (1) | nonce (24) | XChaCha20-Poly1305 ciphertext + tag (n + 16)
//! ```
//!
//! The nonce is random per seal. The associated data is the 5-byte header
//! followed by the UTF-8 key id, so an envelope only opens under the id it
//! was sealed for: swapping sealed blobs between database rows fails.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::keys::{KeyError, SshPrivateKey};
use crate::master_key::MasterKey;

const MAGIC: &[u8; 4] = b"CRSK";
const VERSION_1: u8 = 1;
const HEADER_LEN: usize = 5;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("sealed key is truncated")]
    Truncated,
    #[error("not a controlroom sealed key (bad magic)")]
    BadMagic,
    #[error("unsupported sealed key version {0}")]
    UnsupportedVersion(u8),
    #[error("could not unseal the key: wrong master key, wrong key id, or the data was modified")]
    Decrypt,
    #[error("encryption failed")]
    Encrypt,
    #[error("system random number generator failed: {0}")]
    Random(String),
    #[error(transparent)]
    Key(#[from] KeyError),
}

fn associated_data(header: &[u8], key_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + key_id.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(key_id.as_bytes());
    aad
}

/// Encrypts `plaintext` for storage under `key_id`.
pub fn seal(master: &MasterKey, key_id: &str, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| SealError::Random(e.to_string()))?;

    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4] = VERSION_1;

    let cipher = XChaCha20Poly1305::new(master.as_bytes().into());
    let aad = associated_data(&header, key_id);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| SealError::Encrypt)?;

    let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypts an envelope produced by [`seal`] with the same master key and key id.
pub fn unseal(
    master: &MasterKey,
    key_id: &str,
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, SealError> {
    if sealed.len() < HEADER_LEN {
        return Err(SealError::Truncated);
    }
    let (header, rest) = sealed.split_at(HEADER_LEN);
    if &header[..4] != MAGIC {
        return Err(SealError::BadMagic);
    }
    if header[4] != VERSION_1 {
        return Err(SealError::UnsupportedVersion(header[4]));
    }
    if rest.len() < NONCE_LEN + TAG_LEN {
        return Err(SealError::Truncated);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(master.as_bytes().into());
    let aad = associated_data(header, key_id);
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| SealError::Decrypt)
}

/// Seals an SSH private key (serialized as unencrypted OpenSSH PEM).
pub fn seal_private_key(
    master: &MasterKey,
    key_id: &str,
    key: &SshPrivateKey,
) -> Result<Vec<u8>, SealError> {
    let pem = key.to_openssh()?;
    seal(master, key_id, pem.as_bytes())
}

/// Unseals an SSH private key sealed with [`seal_private_key`].
pub fn unseal_private_key(
    master: &MasterKey,
    key_id: &str,
    sealed: &[u8],
) -> Result<SshPrivateKey, SealError> {
    let plain = unseal(master, key_id, sealed)?;
    let pem = std::str::from_utf8(&plain).map_err(|_| SealError::Decrypt)?;
    Ok(SshPrivateKey::from_openssh(pem)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(byte: u8) -> MasterKey {
        MasterKey::from_bytes([byte; 32])
    }

    #[test]
    fn round_trip() {
        let m = master(7);
        let sealed = seal(&m, "key-1", b"secret material").unwrap();
        assert_eq!(&sealed[..4], b"CRSK");
        assert_eq!(sealed[4], 1);
        assert_eq!(sealed.len(), 5 + 24 + 15 + 16);
        assert_eq!(&*unseal(&m, "key-1", &sealed).unwrap(), b"secret material");
    }

    #[test]
    fn nonces_are_random() {
        let m = master(7);
        let a = seal(&m, "k", b"same").unwrap();
        let b = seal(&m, "k", b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn wrong_master_key_or_id_fails() {
        let sealed = seal(&master(1), "key-1", b"x").unwrap();
        assert!(matches!(
            unseal(&master(2), "key-1", &sealed),
            Err(SealError::Decrypt)
        ));
        assert!(matches!(
            unseal(&master(1), "key-2", &sealed),
            Err(SealError::Decrypt)
        ));
    }

    #[test]
    fn tampering_is_detected() {
        let m = master(3);
        let sealed = seal(&m, "id", b"some secret bytes").unwrap();
        // Flip one bit anywhere after the header: nonce, ciphertext or tag.
        for i in HEADER_LEN..sealed.len() {
            let mut t = sealed.clone();
            t[i] ^= 0x01;
            assert!(
                matches!(unseal(&m, "id", &t), Err(SealError::Decrypt)),
                "bit flip at {i} not detected"
            );
        }
        let mut t = sealed.clone();
        t[4] = 2;
        assert!(matches!(
            unseal(&m, "id", &t),
            Err(SealError::UnsupportedVersion(2))
        ));
        let mut t = sealed.clone();
        t[0] = b'X';
        assert!(matches!(unseal(&m, "id", &t), Err(SealError::BadMagic)));
        assert!(matches!(
            unseal(&m, "id", &sealed[..sealed.len() - 1]),
            Err(SealError::Decrypt)
        ));
        assert!(matches!(
            unseal(&m, "id", &sealed[..20]),
            Err(SealError::Truncated)
        ));
        assert!(matches!(unseal(&m, "id", b"CR"), Err(SealError::Truncated)));
    }

    #[test]
    fn private_key_round_trip() {
        let m = master(9);
        let key = SshPrivateKey::generate_ed25519("controlroom@test").unwrap();
        let sealed = seal_private_key(&m, "01HZ", &key).unwrap();
        let back = unseal_private_key(&m, "01HZ", &sealed).unwrap();
        assert_eq!(back.public_key_line(), key.public_key_line());
        assert!(unseal_private_key(&m, "other", &sealed).is_err());
    }
}
