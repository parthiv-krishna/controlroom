//! Small filesystem helpers shared by the session-dir writers.

use std::collections::HashSet;
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

/// Copies the file or directory tree at `from` to `to`.
///
/// Source symlinks are followed (home-manager links config into the Nix store), each
/// directory at most once, so link cycles terminate. Anything that is neither a regular file
/// nor a directory is skipped. Symlinks at the destination are removed rather than followed:
/// destinations may be writable by the sandbox, which could otherwise redirect this
/// unsandboxed copy elsewhere. Existing files are kept when `overwrite` is false.
pub fn copy_tree(from: &Path, to: &Path, overwrite: bool) -> Result<(), PathIoError> {
    copy_tree_inner(from, to, overwrite, &mut HashSet::new())
}

fn copy_tree_inner(
    from: &Path,
    to: &Path,
    overwrite: bool,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), PathIoError> {
    let meta = at(from, fs::metadata(from))?;
    if to
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        at(to, fs::remove_file(to))?;
    }
    if meta.is_dir() {
        if !visited.insert(at(from, fs::canonicalize(from))?) {
            return Ok(());
        }
        create_private_dir(to)?;
        for entry in at(from, fs::read_dir(from))? {
            let entry = at(from, entry)?;
            copy_tree_inner(
                &entry.path(),
                &to.join(entry.file_name()),
                overwrite,
                visited,
            )?;
        }
    } else if meta.is_file() && (overwrite || !to.exists()) {
        let bytes = at(from, fs::read(from))?;
        write_atomic(to, &bytes, 0o600)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn copy_follows_source_links_once_and_never_destination_links() {
        let t = tempfile::tempdir().expect("tempdir");
        let (src, dst, victim) = (
            t.path().join("src"),
            t.path().join("dst"),
            t.path().join("victim"),
        );
        fs::create_dir_all(src.join("sub")).expect("mkdir");
        fs::create_dir_all(&victim).expect("mkdir");
        fs::write(src.join("sub/f"), "new").expect("write");
        symlink(&src, src.join("sub/loop")).expect("symlink");
        fs::create_dir_all(&dst).expect("mkdir");
        // A planted link must be replaced, not written through.
        symlink(&victim, dst.join("sub")).expect("symlink");

        copy_tree(&src, &dst, true).expect("copy");
        assert_eq!(fs::read_to_string(dst.join("sub/f")).expect("read"), "new");
        assert!(
            !dst.join("sub")
                .symlink_metadata()
                .expect("meta")
                .file_type()
                .is_symlink()
        );
        assert!(fs::read_dir(&victim).expect("read_dir").next().is_none());
    }
}
