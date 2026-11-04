//! `fs.list`: directory listings for the new-session dialog and the file browser, with the host
//! user's own access (not a workspace sandbox's).

use std::time::UNIX_EPOCH;

use controlroom_protocol::daemon::{FsEntry, FsEntryKind, FsListParams, FsListResult};
use controlroom_protocol::{ErrorData, RemotePath, RpcError, UnixMillis};
use controlroom_sandbox::AbsPath;

use crate::dirs::expand_home;

fn fs_error(path: &RemotePath, detail: impl std::fmt::Display) -> RpcError {
    RpcError::new(
        ErrorData::FsError {
            path: path.clone(),
            detail: detail.to_string(),
        },
        format!("{path}: {detail}"),
    )
}

/// Lists `params.path` (`~` expanded): directories (and links to them), plus everything else
/// with `include_files`; sorted by name.
pub fn list(home: &AbsPath, params: &FsListParams) -> Result<FsListResult, RpcError> {
    let requested = &params.path;
    let dir = expand_home(requested.as_str(), home).map_err(|e| fs_error(requested, e))?;
    let read = std::fs::read_dir(dir.as_path()).map_err(|e| fs_error(requested, e))?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let kind = if meta.is_dir() {
            FsEntryKind::Dir
        } else if meta.is_file() {
            FsEntryKind::File
        } else if meta.file_type().is_symlink() {
            FsEntryKind::Symlink
        } else {
            FsEntryKind::Other
        };
        let wanted = params.include_files
            || kind == FsEntryKind::Dir
            || (kind == FsEntryKind::Symlink && entry.path().is_dir());
        if !wanted {
            continue;
        }
        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size: (kind == FsEntryKind::File).then_some(meta.len()),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| UnixMillis(i64::try_from(d.as_millis()).unwrap_or(i64::MAX))),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(FsListResult {
        path: RemotePath::from(dir.as_str()),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dirs_links_and_files() {
        let t = tempfile::tempdir().unwrap();
        let home = AbsPath::from_path(t.path()).unwrap();
        std::fs::create_dir_all(t.path().join("src/b")).unwrap();
        std::fs::create_dir(t.path().join("src/a")).unwrap();
        std::fs::write(t.path().join("src/notes.txt"), "hi").unwrap();
        std::os::unix::fs::symlink(t.path().join("src/a"), t.path().join("src/link")).unwrap();
        let params = |include_files| FsListParams {
            path: "~/src".into(),
            include_files,
        };
        let dirs = list(&home, &params(false)).unwrap();
        assert_eq!(dirs.path.as_str(), format!("{home}/src"));
        let names: Vec<&str> = dirs.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "link"]);
        assert_eq!(dirs.entries[2].kind, FsEntryKind::Symlink);

        let all = list(&home, &params(true)).unwrap();
        let notes = all.entries.iter().find(|e| e.name == "notes.txt").unwrap();
        assert_eq!((notes.kind, notes.size), (FsEntryKind::File, Some(2)));

        let missing = list(
            &home,
            &FsListParams {
                path: "~/nope".into(),
                include_files: false,
            },
        )
        .unwrap_err();
        assert!(matches!(missing.data, Some(ErrorData::FsError { .. })));
    }
}
