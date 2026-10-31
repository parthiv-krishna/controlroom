//! The master key that seals SSH private keys (spec §12, §14: the required master key).
//!
//! The web server loads it through systemd `LoadCredential=`, i.e. from
//! `$CREDENTIALS_DIRECTORY/<name>`. A master key file holds exactly 32 bytes of
//! key material in one of these encodings:
//!
//! - **raw**: the file is exactly 32 bytes long (e.g. `head -c 32 /dev/urandom`);
//! - **hex**: 64 hex digits (e.g. `openssl rand -hex 32`);
//! - **base64**: standard alphabet, 44 characters padded or 43 unpadded
//!   (e.g. `openssl rand -base64 32`).
//!
//! Leading and trailing ASCII whitespace (such as a final newline) is ignored for
//! the text encodings. The encodings cannot be confused: text forms are never 32
//! bytes long. An all-zero key is rejected as an obvious placeholder.

use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

pub const MASTER_KEY_LEN: usize = 32;

/// A 32-byte symmetric key, zeroized on drop.
#[derive(Clone)]
pub struct MasterKey(Zeroizing<[u8; MASTER_KEY_LEN]>);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MasterKeyError {
    #[error(
        "$CREDENTIALS_DIRECTORY is not set; pass the master key with systemd LoadCredential={name}:<path>"
    )]
    NoCredentialsDirectory { name: String },
    #[error("master key file {path} not found")]
    NotFound { path: PathBuf },
    #[error("could not read master key file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "master key{} has an invalid format: expected 32 raw bytes, 64 hex digits, or base64 of 32 bytes ({detail})",
        path.as_ref().map(|p| format!(" file {}", p.display())).unwrap_or_default()
    )]
    Format {
        path: Option<PathBuf>,
        detail: String,
    },
    #[error("master key is all zeros; generate a random one (e.g. `openssl rand -base64 32`)")]
    AllZero,
}

impl MasterKey {
    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.0
    }

    /// Parses master key file contents (see the module docs for accepted encodings).
    pub fn parse(contents: &[u8]) -> Result<Self, MasterKeyError> {
        let key = parse_key_material(contents)
            .map_err(|detail| MasterKeyError::Format { path: None, detail })?;
        if key.iter().all(|&b| b == 0) {
            return Err(MasterKeyError::AllZero);
        }
        Ok(Self(key))
    }

    /// Reads and parses a master key file.
    pub fn load_file(path: &Path) -> Result<Self, MasterKeyError> {
        let contents = Zeroizing::new(std::fs::read(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                MasterKeyError::NotFound {
                    path: path.to_owned(),
                }
            } else {
                MasterKeyError::Read {
                    path: path.to_owned(),
                    source,
                }
            }
        })?);
        Self::parse(&contents).map_err(|e| match e {
            MasterKeyError::Format { detail, .. } => MasterKeyError::Format {
                path: Some(path.to_owned()),
                detail,
            },
            other => other,
        })
    }

    /// Loads the systemd credential `name` from `$CREDENTIALS_DIRECTORY`.
    pub fn load_credential(name: &str) -> Result<Self, MasterKeyError> {
        let dir = std::env::var_os("CREDENTIALS_DIRECTORY").ok_or_else(|| {
            MasterKeyError::NoCredentialsDirectory {
                name: name.to_owned(),
            }
        })?;
        Self::load_file(&Path::new(&dir).join(name))
    }
}

fn parse_key_material(contents: &[u8]) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, String> {
    let mut key = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    if contents.len() == MASTER_KEY_LEN {
        key.copy_from_slice(contents);
        return Ok(key);
    }
    let text = contents.trim_ascii();
    if text.is_empty() {
        return Err("file is empty".into());
    }
    if !text.is_ascii() {
        return Err(format!(
            "{} bytes of binary data; raw keys must be exactly 32 bytes",
            contents.len()
        ));
    }
    let decoded = if text.len() == 2 * MASTER_KEY_LEN && text.iter().all(u8::is_ascii_hexdigit) {
        data_encoding::HEXLOWER_PERMISSIVE.decode(text)
    } else if text.len() == 44 {
        data_encoding::BASE64.decode(text)
    } else if text.len() == 43 {
        data_encoding::BASE64_NOPAD.decode(text)
    } else {
        return Err(format!("{} characters of text", text.len()));
    };
    let decoded = Zeroizing::new(decoded.map_err(|e| format!("decoding failed: {e}"))?);
    if decoded.len() != MASTER_KEY_LEN {
        return Err(format!("decodes to {} bytes", decoded.len()));
    }
    key.copy_from_slice(&decoded);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8 * 7 + 1;
        }
        k
    }

    #[test]
    fn raw_bytes() {
        let k = MasterKey::parse(&sample()).unwrap();
        assert_eq!(k.as_bytes(), &sample());
    }

    #[test]
    fn raw_bytes_that_look_like_whitespace_are_kept() {
        let mut raw = sample();
        raw[0] = b'\n';
        raw[31] = b' ';
        assert_eq!(MasterKey::parse(&raw).unwrap().as_bytes(), &raw);
    }

    #[test]
    fn hex_upper_and_lower_with_newline() {
        let hex = data_encoding::HEXLOWER.encode(&sample());
        let k = MasterKey::parse(format!("{hex}\n").as_bytes()).unwrap();
        assert_eq!(k.as_bytes(), &sample());
        let k = MasterKey::parse(hex.to_uppercase().as_bytes()).unwrap();
        assert_eq!(k.as_bytes(), &sample());
    }

    #[test]
    fn base64_padded_and_unpadded() {
        let b64 = data_encoding::BASE64.encode(&sample());
        assert_eq!(b64.len(), 44);
        let k = MasterKey::parse(format!("  {b64}\r\n").as_bytes()).unwrap();
        assert_eq!(k.as_bytes(), &sample());
        let k = MasterKey::parse(b64.trim_end_matches('=').as_bytes()).unwrap();
        assert_eq!(k.as_bytes(), &sample());
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            &b""[..],
            b"\n",
            b"too short",
            &[0xffu8; 31],
            &[0xffu8; 33],
            // 64 chars but not hex.
            b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            // base64 of 33 bytes is 44 chars.
            b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(
                matches!(MasterKey::parse(bad), Err(MasterKeyError::Format { .. })),
                "{bad:?}"
            );
        }
        assert!(matches!(
            MasterKey::parse(&[0u8; 32]),
            Err(MasterKeyError::AllZero)
        ));
    }

    #[test]
    fn load_file_errors_name_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(matches!(
            MasterKey::load_file(&missing),
            Err(MasterKeyError::NotFound { .. })
        ));
        let bad = dir.path().join("bad");
        std::fs::write(&bad, "hello").unwrap();
        let err = MasterKey::load_file(&bad).unwrap_err().to_string();
        assert!(err.contains(&bad.display().to_string()), "{err}");

        let good = dir.path().join("good");
        std::fs::write(&good, data_encoding::BASE64.encode(&sample())).unwrap();
        assert_eq!(MasterKey::load_file(&good).unwrap().as_bytes(), &sample());
    }
}
