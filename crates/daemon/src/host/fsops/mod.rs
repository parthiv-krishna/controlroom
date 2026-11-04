//! The file and search ops of a tool call (`read`, `read_bytes`, `stat`, `write`, `ls`,
//! `find`, `grep`), as plain functions over the file system.
//!
//! They run inside the per-workspace fs worker ([`super::worker`]), under the workspace's
//! nono profile, so a path outside what the sandbox may touch fails with the OS's permission
//! error (`fs_error`) rather than being checked here. Paths resolve like the agent writes
//! them: `~` is the host user's home, a relative path is relative to the workspace root, and
//! `.`/`..` are resolved lexically (symlinks are left alone, so results name what was asked).

mod lines;
mod read;
mod search;
mod stat;
mod write;

use controlroom_protocol::host::{HostOp, HostResult};
use controlroom_protocol::{ErrorData, RemotePath, RpcError};

pub use lines::LineIndexCache;
pub use read::sha256_hex;
pub use write::content_bytes;

/// Where an op runs: the workspace root and the home `~` stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The workspace root (absolute).
    pub root: String,
    /// The host user's home directory (absolute).
    pub home: String,
}

impl Context {
    /// Resolves an op's path to an absolute, lexically normalized path.
    pub fn resolve(&self, path: &str) -> String {
        let joined = if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            format!("{}/{rest}", self.home)
        } else if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("{}/{path}", self.root)
        };
        normalize(&joined)
    }

    /// [`resolve`](Self::resolve) of an optional path, defaulting to the root.
    pub fn resolve_or_root(&self, path: Option<&RemotePath>) -> String {
        path.map_or_else(|| normalize(&self.root), |p| self.resolve(p.as_str()))
    }
}

/// Removes empty and `.` components and applies `..` (never above `/`).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Runs a file or search op. `bash` and job ops are not file ops: `invalid_params`.
pub fn run(ctx: &Context, cache: &LineIndexCache, op: HostOp) -> Result<HostResult, RpcError> {
    match op {
        HostOp::Read {
            path,
            offset,
            limit,
        } => read::read(&ctx.resolve(path.as_str()), cache, offset, limit),
        HostOp::ReadBytes {
            path,
            offset,
            length,
        } => read::read_bytes(&ctx.resolve(path.as_str()), offset, length),
        HostOp::Stat { path } => Ok(stat::stat(&ctx.resolve(path.as_str()))),
        HostOp::Write {
            path,
            content,
            encoding,
            expect_sha256,
        } => write::write(
            &ctx.resolve(path.as_str()),
            &content,
            encoding,
            expect_sha256.as_deref(),
        ),
        HostOp::Ls { path, limit } => stat::ls(&ctx.resolve_or_root(path.as_ref()), limit),
        HostOp::Find {
            pattern,
            path,
            limit,
        } => search::find(&ctx.resolve_or_root(path.as_ref()), &pattern, limit),
        HostOp::Grep {
            pattern,
            path,
            glob,
            ignore_case,
            literal,
            context,
            limit,
        } => search::grep(
            &ctx.resolve_or_root(path.as_ref()),
            &search::GrepQuery {
                pattern,
                glob,
                ignore_case,
                literal,
                context: context.unwrap_or(0),
                limit,
            },
        ),
        other => Err(invalid(format!(
            "{} is not a file op",
            serde_json::to_value(&other)
                .ok()
                .and_then(|v| v["op"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "this op".to_owned())
        ))),
    }
}

/// An `fs_error` for `path`.
pub(crate) fn fs_error(path: &str, e: impl std::fmt::Display) -> RpcError {
    RpcError::new(
        ErrorData::FsError {
            path: RemotePath::from(path),
            detail: e.to_string(),
        },
        format!("{path}: {e}"),
    )
}

/// An `invalid_params` error.
pub(crate) fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::InvalidParams, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_against_root_and_home() {
        let ctx = Context {
            root: "/home/alice/src/api".into(),
            home: "/home/alice".into(),
        };
        assert_eq!(ctx.resolve("main.rs"), "/home/alice/src/api/main.rs");
        assert_eq!(ctx.resolve("./a/../b//c"), "/home/alice/src/api/b/c");
        assert_eq!(ctx.resolve("~"), "/home/alice");
        assert_eq!(ctx.resolve("~/.gitconfig"), "/home/alice/.gitconfig");
        assert_eq!(ctx.resolve("/etc/../../x"), "/x");
        assert_eq!(ctx.resolve_or_root(None), "/home/alice/src/api");
    }
}
