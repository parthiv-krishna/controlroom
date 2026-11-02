//! Secrets: reading credential files (the OIDC client secret), writing private files (the dev
//! token), random tokens and constant-time comparison.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use data_encoding::BASE64URL_NOPAD;
use subtle::ConstantTimeEq;

/// Why a secret file couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The file couldn't be read.
    #[error("reading secret file {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// Why.
        source: std::io::Error,
    },
    /// The file is empty.
    #[error("secret file {0} is empty")]
    Empty(String),
}

/// Reads a text secret, trimming the trailing newline editors and `echo` add.
pub fn read_text_secret(path: &Path) -> Result<String, SecretError> {
    let text = std::fs::read_to_string(path).map_err(|source| SecretError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Err(SecretError::Empty(path.display().to_string()));
    }
    Ok(trimmed.to_owned())
}

/// Writes `contents` to `path` with mode 0600, replacing any existing file atomically.
pub fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Creates a directory (mode 0700) and its parents if missing.
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// `N` random bytes from the OS.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the OS random number generator works");
    bytes
}

/// A random URL-safe token with 256 bits of entropy.
pub fn random_token() -> String {
    BASE64URL_NOPAD.encode(&random_bytes::<32>())
}

/// Constant-time string equality.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn private_files_are_0600_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        write_private_file(&path, b"one").unwrap();
        write_private_file(&path, b"two").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn text_secrets_are_trimmed_and_required() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, "s3cret\n").unwrap();
        assert_eq!(read_text_secret(&path).unwrap(), "s3cret");
        std::fs::write(&path, "\n").unwrap();
        assert!(matches!(
            read_text_secret(&path),
            Err(SecretError::Empty(_))
        ));
        assert!(matches!(
            read_text_secret(&dir.path().join("missing")),
            Err(SecretError::Read { .. })
        ));
    }

    #[test]
    fn tokens_and_comparison() {
        assert_ne!(random_token(), random_token());
        assert_eq!(random_token().len(), 43);
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
