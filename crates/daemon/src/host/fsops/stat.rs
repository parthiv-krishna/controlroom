//! `stat` and `ls`.

use std::fs::Metadata;
use std::time::UNIX_EPOCH;

use controlroom_protocol::daemon::{FsEntry, FsEntryKind};
use controlroom_protocol::host::HostResult;
use controlroom_protocol::{RemotePath, RpcError, UnixMillis};

use super::fs_error;

/// Default and largest `ls` limit.
const LS_LIMIT: u64 = 500;

fn kind(meta: &Metadata) -> FsEntryKind {
    let t = meta.file_type();
    if t.is_dir() {
        FsEntryKind::Dir
    } else if t.is_file() {
        FsEntryKind::File
    } else if t.is_symlink() {
        FsEntryKind::Symlink
    } else {
        FsEntryKind::Other
    }
}

fn mtime(meta: &Metadata) -> Option<UnixMillis> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| UnixMillis(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
}

/// Whether `path` exists and what it is (following symlinks; a dangling link is a
/// `symlink`).
pub fn stat(path: &str) -> HostResult {
    let meta = std::fs::metadata(path).or_else(|_| std::fs::symlink_metadata(path));
    match meta {
        Ok(meta) => {
            let kind = kind(&meta);
            HostResult::Stat {
                path: RemotePath::from(path),
                exists: true,
                size: (kind == FsEntryKind::File).then_some(meta.len()),
                mtime: mtime(&meta),
                kind: Some(kind),
            }
        }
        Err(_) => HostResult::Stat {
            path: RemotePath::from(path),
            exists: false,
            kind: None,
            size: None,
            mtime: None,
        },
    }
}

/// A directory's entries sorted by name, dot files included, at most `limit`.
pub fn ls(path: &str, limit: Option<u64>) -> Result<HostResult, RpcError> {
    let limit = limit.unwrap_or(LS_LIMIT).clamp(1, LS_LIMIT * 20);
    let read = std::fs::read_dir(path).map_err(|e| fs_error(path, e))?;
    let mut entries: Vec<FsEntry> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| fs_error(path, e))?;
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mut kind = kind(&meta);
        if kind == FsEntryKind::Symlink && entry.path().is_dir() {
            kind = FsEntryKind::Dir;
        }
        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            size: (kind == FsEntryKind::File).then_some(meta.len()),
            mtime: mtime(&meta),
            kind,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let total = entries.len() as u64;
    entries.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(HostResult::Ls {
        path: RemotePath::from(path),
        entries,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_and_ls() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("b.txt"), "hi").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let HostResult::Ls { entries, total, .. } = ls(root, Some(2)).unwrap() else {
            panic!()
        };
        assert_eq!(total, 3);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, [".hidden", "a"]);
        assert_eq!(entries[1].kind, FsEntryKind::Dir);

        let HostResult::Stat {
            exists, kind, size, ..
        } = stat(&format!("{root}/b.txt"))
        else {
            panic!()
        };
        assert_eq!((exists, kind, size), (true, Some(FsEntryKind::File), Some(2)));
        let HostResult::Stat { exists, .. } = stat(&format!("{root}/nope")) else {
            panic!()
        };
        assert!(!exists);
        assert!(ls(&format!("{root}/nope"), None).is_err());
    }
}
