//! Small filesystem helpers shared by the session-dir writers.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// An I/O error with the path it concerns.
#[derive(Debug, thiserror::Error)]
#[error("{path}: {source}")]
pub struct PathIoError {
    /// The path.
    pub path: PathBuf,
    /// The underlying error.
    #[source]
    pub source: io::Error,
}

/// Attaches `path` to an I/O result.
pub fn at<T>(path: &Path, r: io::Result<T>) -> Result<T, PathIoError> {
    r.map_err(|source| PathIoError {
        path: path.to_owned(),
        source,
    })
}

/// Creates `dir` and missing parents with mode 0700.
pub fn create_private_dir(dir: &Path) -> Result<(), PathIoError> {
    at(
        dir,
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir),
    )
}

/// Writes `bytes` to `path` atomically (temporary file + rename) with `mode`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), PathIoError> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{}.tmp", name.unwrap_or_default()));
    let write = || -> io::Result<()> {
        let _ = fs::remove_file(&tmp);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    };
    at(path, write())
}

/// Copies the file or directory tree at `from` to `to`, following symlinks and skipping
/// anything that is neither a regular file nor a directory. Existing files are kept when
/// `overwrite` is false.
pub fn copy_tree(from: &Path, to: &Path, overwrite: bool) -> Result<(), PathIoError> {
    let meta = at(from, fs::metadata(from))?;
    if meta.is_dir() {
        create_private_dir(to)?;
        for entry in at(from, fs::read_dir(from))? {
            let entry = at(from, entry)?;
            copy_tree(&entry.path(), &to.join(entry.file_name()), overwrite)?;
        }
    } else if meta.is_file() && (overwrite || !to.exists()) {
        let bytes = at(from, fs::read(from))?;
        write_atomic(to, &bytes, 0o600)?;
    }
    Ok(())
}
