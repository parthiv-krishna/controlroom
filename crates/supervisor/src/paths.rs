//! Files of one supervised process, and the ids and paths it accepts.

use std::path::{Path, PathBuf};

use crate::proto::SupervisorInfo;

/// Longest unix socket path: `sun_path` is 108 bytes including the NUL.
pub const SUN_PATH_MAX: usize = 107;

/// Where a supervisor keeps its files.
///
/// `dir` holds `transcript.jsonl` (plus its `transcript.idx`), `supervisor.json` and
/// `supervisor.log`. The socket lives elsewhere because of the `sun_path` limit: the
/// daemon uses a short scheme such as `<state>/run/<id>.sock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The process directory (created with mode 0700 if missing).
    pub dir: PathBuf,
    /// The socket path (at most [`SUN_PATH_MAX`] bytes).
    pub socket: PathBuf,
}

/// Why an id or paths are unusable.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    /// The id has characters outside `[A-Za-z0-9_-]` or is not 1-64 long.
    #[error("invalid id {0:?}: use 1-64 characters from [A-Za-z0-9_-]")]
    InvalidId(String),
    /// A path is relative.
    #[error("{0:?} is not absolute")]
    Relative(PathBuf),
    /// The socket path is too long for a unix socket.
    #[error(
        "socket path {path:?} is {len} bytes, over the {SUN_PATH_MAX}-byte unix socket limit; \
         use a shorter path"
    )]
    SocketTooLong {
        /// The path.
        path: PathBuf,
        /// Its length in bytes.
        len: usize,
    },
}

impl Paths {
    /// The transcript.
    pub fn transcript(&self) -> PathBuf {
        self.dir.join("transcript.jsonl")
    }

    /// The running supervisor's [`SupervisorInfo`], written atomically once it is
    /// ready and removed on a clean exit.
    pub fn info_file(&self) -> PathBuf {
        info_file(&self.dir)
    }

    /// Stderr of a detached supervisor.
    pub fn log_file(&self) -> PathBuf {
        self.dir.join("supervisor.log")
    }

    /// Checks that both paths are absolute and the socket path fits `sun_path`.
    pub fn validate(&self) -> Result<(), PathError> {
        for path in [&self.dir, &self.socket] {
            if !path.is_absolute() {
                return Err(PathError::Relative(path.clone()));
            }
        }
        let len = self.socket.as_os_str().len();
        if len > SUN_PATH_MAX {
            return Err(PathError::SocketTooLong {
                path: self.socket.clone(),
                len,
            });
        }
        Ok(())
    }

    /// Creates `dir` (and its parents) with mode 0700.
    pub(crate) fn create_dir(&self) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
    }
}

/// `<dir>/supervisor.json`.
fn info_file(dir: &Path) -> PathBuf {
    dir.join("supervisor.json")
}

/// Reads `supervisor.json` from a process directory, if a supervisor left one.
///
/// Its presence doesn't prove the supervisor is alive: connect to the socket and
/// send `hello` to find out.
pub fn read_info(dir: &Path) -> Option<SupervisorInfo> {
    let bytes = std::fs::read(info_file(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Checks a process id, which becomes part of file names.
pub fn validate_id(id: &str) -> Result<(), PathError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(PathError::InvalidId(id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        validate_id("01JABCDEFGHJKMNPQRSTVWXYZ0").unwrap();
        validate_id("a_b-c").unwrap();
        for bad in ["", "../x", "a/b", "a.b", &"x".repeat(65)] {
            assert!(validate_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn layout_and_limits() {
        let paths = Paths {
            dir: "/s/sessions/abc".into(),
            socket: "/s/run/abc.sock".into(),
        };
        paths.validate().unwrap();
        assert_eq!(
            paths.transcript(),
            Path::new("/s/sessions/abc/transcript.jsonl")
        );
        assert_eq!(
            paths.info_file(),
            Path::new("/s/sessions/abc/supervisor.json")
        );

        let long = Paths {
            socket: PathBuf::from(format!("/{}", "x".repeat(SUN_PATH_MAX))),
            ..paths.clone()
        };
        assert!(matches!(
            long.validate(),
            Err(PathError::SocketTooLong { len: 108, .. })
        ));
        let exact = Paths {
            socket: PathBuf::from(format!("/{}", "x".repeat(SUN_PATH_MAX - 1))),
            ..paths.clone()
        };
        exact.validate().unwrap();
        let relative = Paths {
            dir: "rel".into(),
            ..paths
        };
        assert!(matches!(relative.validate(), Err(PathError::Relative(_))));
    }
}
