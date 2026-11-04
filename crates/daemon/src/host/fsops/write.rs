//! `write`: atomic replacement (temporary file, fsync, rename, fsync of the directory) with an
//! optional compare-and-swap on the current content's SHA-256.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use controlroom_protocol::host::{ContentEncoding, HostResult};
use controlroom_protocol::{ErrorData, RemotePath, RpcError};

use super::read::sha256_hex;
use super::{fs_error, invalid};

/// The bytes a write puts in place.
pub fn content_bytes(content: &str, encoding: ContentEncoding) -> Result<Vec<u8>, RpcError> {
    match encoding {
        ContentEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        ContentEncoding::Base64 => STANDARD
            .decode(content)
            .map_err(|e| invalid(format!("content is not base64: {e}"))),
    }
}

/// The SHA-256 (hex) of the file at `path`; `None` if it doesn't exist.
pub fn file_sha256(path: &str) -> Result<Option<String>, RpcError> {
    match std::fs::read(path) {
        Ok(data) => Ok(Some(sha256_hex(&data))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(fs_error(path, e)),
    }
}

/// Replaces the file at `path`, creating parent directories. With `expect_sha256`, only if
/// its current content has that hash (else `conflict`, and nothing is written).
pub fn write(
    path: &str,
    content: &str,
    encoding: ContentEncoding,
    expect_sha256: Option<&str>,
) -> Result<HostResult, RpcError> {
    let data = content_bytes(content, encoding)?;
    if let Some(expected) = expect_sha256
        && file_sha256(path)?.as_deref() != Some(expected)
    {
        return Err(RpcError::new(
            ErrorData::Conflict {
                path: RemotePath::from(path),
            },
            format!("{path} changed since it was read; read it again and retry"),
        ));
    }
    let target = Path::new(path);
    let parent = target
        .parent()
        .ok_or_else(|| invalid(format!("{path} is not a file path")))?;
    std::fs::create_dir_all(parent).map_err(|e| fs_error(path, e))?;
    let mode = std::fs::metadata(target).map(|m| m.permissions().mode() & 0o7777);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| invalid(format!("{path} is not a file path")))?;
    let tmp = parent.join(format!(".{name}.cr-{}", ulid::Ulid::generate()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)?;
        file.write_all(&data)?;
        if let Ok(mode) = mode {
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp, target)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(fs_error(path, e));
    }
    Ok(HostResult::Write {
        path: RemotePath::from(path),
        bytes: data.len() as u64,
        sha256: sha256_hex(&data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_atomically_and_compares_and_swaps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/f.txt");
        let path = path.to_str().unwrap();
        let HostResult::Write { bytes, sha256, .. } =
            write(path, "one", ContentEncoding::Utf8, None).unwrap()
        else {
            panic!()
        };
        assert_eq!(bytes, 3);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o750)).unwrap();

        let stale = sha256_hex(b"something else");
        let err = write(path, "two", ContentEncoding::Utf8, Some(&stale)).unwrap_err();
        assert!(matches!(err.data, Some(ErrorData::Conflict { .. })));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "one");

        write(path, "dHdv", ContentEncoding::Base64, Some(&sha256)).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "two");
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
        let leftovers = std::fs::read_dir(dir.path().join("sub")).unwrap().count();
        assert_eq!(leftovers, 1);
    }
}
