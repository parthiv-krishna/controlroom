//! `fs.list`: a directory listing with the host user's own access (new-session dialog, file
//! browser), and the entry types the `ls` op shares.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{RemotePath, UnixMillis};

/// Params of `fs.list`, for the working-directory picker. Lists with the daemon user's access,
/// not a session sandbox's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsListParams {
    /// The directory; `~` is expanded.
    pub path: RemotePath,
    /// Also list non-directories; default: directories only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_files: bool,
}

/// The kind of a directory entry (symlinks are not followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FsEntryKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link (listed in directories-only mode when it resolves to a directory).
    Symlink,
    /// A socket, FIFO or device.
    Other,
    /// A kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsEntry {
    /// The file name.
    pub name: String,
    /// Its kind.
    pub kind: FsEntryKind,
    /// Size in bytes, for files.
    pub size: Option<u64>,
    /// Last modification time, when known.
    pub mtime: Option<UnixMillis>,
}

/// Result of `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsListResult {
    /// The listed directory, with `~` expanded.
    pub path: RemotePath,
    /// Its entries, sorted by name.
    pub entries: Vec<FsEntry>,
}
