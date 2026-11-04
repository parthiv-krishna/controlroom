//! The spawn spec: what a supervisor runs and how.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use controlroom_transcript::{ProcessKind, StdinMode};
use serde::{Deserialize, Serialize};

/// Default for [`SpawnSpec::max_line_bytes`].
pub const DEFAULT_MAX_LINE_BYTES: u64 = 16 << 20;

/// Upper bound for [`SpawnSpec::max_line_bytes`], so every record stays well under
/// the transcript's record limit even with worst-case JSON escaping.
pub const MAX_LINE_BYTES: u64 = 16 << 20;

/// What a supervisor runs and how. Serialized as JSON on the `supervise` command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// What kind of process this is. A `pty` child runs on a terminal (see the crate docs).
    pub kind: ProcessKind,
    /// The program to execute.
    pub exec: Exec,
    /// Absolute working directory of the child.
    pub cwd: PathBuf,
    /// File of `KEY=VALUE` lines added to the child's environment (see [`parse_env`]).
    /// Secrets go here, never into argv (which other processes can read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<PathBuf>,
    /// What the child's stdin is connected to.
    #[serde(default)]
    pub stdin: StdinMode,
    /// Stop the child (SIGTERM, then SIGKILL after a grace period) after this long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Longest stdout/stderr line logged in full (default [`DEFAULT_MAX_LINE_BYTES`]);
    /// longer lines are cut and marked `truncated`. Also the longest accepted `write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_line_bytes: Option<u64>,
    /// Opaque labels recorded in the `meta` record (for example `driver=pi`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Also append every stdout/stderr line, as [`plain_line`](crate::plain::plain_line)
    /// text, to this file (created if missing, never truncated): line `n` of the file is the
    /// `n`th output record. Absolute; its directory must exist. Jobs use it for the log the
    /// agent reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_log: Option<PathBuf>,
}

/// The program a supervisor executes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exec {
    /// A plain argv, executed directly (`argv[0]` is looked up in `PATH`). Wrappers
    /// such as `nono run ... -- $SHELL -lc '<cmd>'` are passed this way.
    Argv(Vec<String>),
    /// A command line run as `$SHELL -lc '<command>'` (`/bin/sh` if `SHELL` is unset),
    /// so the login environment sets up `PATH`.
    Shell(String),
}

/// Why a spawn spec is unusable.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpecError {
    /// The kind is unknown.
    #[error("unsupported process kind {0:?}")]
    UnsupportedKind(ProcessKind),
    /// `exec` is empty or contains NUL bytes.
    #[error("invalid exec: {0}")]
    InvalidExec(&'static str),
    /// `cwd` is not absolute.
    #[error("cwd {0:?} is not absolute")]
    RelativeCwd(PathBuf),
    /// `env_file` is not absolute.
    #[error("env file {0:?} is not absolute")]
    RelativeEnvFile(PathBuf),
    /// `plain_log` was given for a `pty` (terminal output is not line-based).
    #[error("a pty has no plain log")]
    PtyPlainLog,
    /// `plain_log` is not absolute.
    #[error("plain log {0:?} is not absolute")]
    RelativePlainLog(PathBuf),
    /// The stdin mode is unknown.
    #[error("unsupported stdin mode")]
    UnsupportedStdin,
    /// `timeout_ms` is 0.
    #[error("timeout_ms must be positive")]
    ZeroTimeout,
    /// `max_line_bytes` is 0 or above [`MAX_LINE_BYTES`].
    #[error("max_line_bytes must be between 1 and {MAX_LINE_BYTES}")]
    LineCap,
}

impl SpawnSpec {
    /// Checks the spec without touching the file system.
    pub fn validate(&self) -> Result<(), SpecError> {
        match self.kind {
            ProcessKind::Agent | ProcessKind::Job => {}
            ProcessKind::Pty if self.plain_log.is_some() => return Err(SpecError::PtyPlainLog),
            ProcessKind::Pty => {}
            kind @ ProcessKind::Unknown => {
                return Err(SpecError::UnsupportedKind(kind));
            }
        }
        match &self.exec {
            Exec::Argv(argv) if argv.is_empty() || argv[0].is_empty() => {
                return Err(SpecError::InvalidExec("empty argv"));
            }
            Exec::Argv(argv) if argv.iter().any(|a| a.contains('\0')) => {
                return Err(SpecError::InvalidExec("NUL in argv"));
            }
            Exec::Shell(command) if command.trim().is_empty() => {
                return Err(SpecError::InvalidExec("empty shell command"));
            }
            Exec::Shell(command) if command.contains('\0') => {
                return Err(SpecError::InvalidExec("NUL in shell command"));
            }
            Exec::Argv(_) | Exec::Shell(_) => {}
        }
        if !self.cwd.is_absolute() {
            return Err(SpecError::RelativeCwd(self.cwd.clone()));
        }
        if let Some(env_file) = &self.env_file
            && !env_file.is_absolute()
        {
            return Err(SpecError::RelativeEnvFile(env_file.clone()));
        }
        if let Some(plain_log) = &self.plain_log
            && !plain_log.is_absolute()
        {
            return Err(SpecError::RelativePlainLog(plain_log.clone()));
        }
        if self.stdin == StdinMode::Unknown {
            return Err(SpecError::UnsupportedStdin);
        }
        if self.timeout_ms == Some(0) {
            return Err(SpecError::ZeroTimeout);
        }
        if let Some(max) = self.max_line_bytes
            && !(1..=MAX_LINE_BYTES).contains(&max)
        {
            return Err(SpecError::LineCap);
        }
        Ok(())
    }

    /// The line cap in effect.
    pub fn line_cap(&self) -> u64 {
        self.max_line_bytes.unwrap_or(DEFAULT_MAX_LINE_BYTES)
    }

    /// The argv to execute, with `shell` resolved against `$SHELL`.
    pub fn argv(&self, shell: Option<&str>) -> Vec<String> {
        match &self.exec {
            Exec::Argv(argv) => argv.clone(),
            Exec::Shell(command) => {
                let shell = shell.filter(|s| !s.is_empty()).unwrap_or("/bin/sh");
                vec![shell.to_owned(), "-lc".to_owned(), command.clone()]
            }
        }
    }
}

/// Reads an env file; see [`parse_env`].
pub fn read_env_file(path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    use anyhow::Context as _;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading env file {}", path.display()))?;
    parse_env(&content).with_context(|| format!("in env file {}", path.display()))
}

/// Parses `KEY=VALUE` lines. Blank lines and lines starting with `#` are skipped;
/// values are taken literally (no quoting or expansion), as with `docker --env-file`.
pub fn parse_env(content: &str) -> anyhow::Result<Vec<(String, String)>> {
    let mut vars = Vec::new();
    for (n, line) in content.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            anyhow::bail!("line {}: expected KEY=VALUE", n + 1);
        };
        if key.is_empty() || key.contains(|c: char| c.is_whitespace() || c == '\0') {
            anyhow::bail!("line {}: invalid variable name {key:?}", n + 1);
        }
        if value.contains('\0') {
            anyhow::bail!("line {}: value contains NUL", n + 1);
        }
        vars.push((key.to_owned(), value.to_owned()));
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(exec: Exec) -> SpawnSpec {
        SpawnSpec {
            kind: ProcessKind::Agent,
            exec,
            cwd: "/home/alice".into(),
            env_file: None,
            stdin: StdinMode::Pipe,
            timeout_ms: None,
            max_line_bytes: None,
            labels: BTreeMap::new(),
            plain_log: None,
        }
    }

    #[test]
    fn json_shape() {
        let json = r#"{"kind":"job","exec":{"argv":["make","test"]},"cwd":"/src","stdin":"null","timeout_ms":5000}"#;
        let parsed: SpawnSpec = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.kind, ProcessKind::Job);
        assert_eq!(parsed.exec, Exec::Argv(vec!["make".into(), "test".into()]));
        assert_eq!(parsed.stdin, StdinMode::Null);
        assert_eq!(parsed.line_cap(), DEFAULT_MAX_LINE_BYTES);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);

        let shell: SpawnSpec =
            serde_json::from_str(r#"{"kind":"agent","exec":{"shell":"pi --mode rpc"},"cwd":"/"}"#)
                .unwrap();
        assert_eq!(shell.stdin, StdinMode::Pipe);
        assert_eq!(
            shell.argv(Some("/bin/zsh")),
            ["/bin/zsh", "-lc", "pi --mode rpc"]
        );
        assert_eq!(shell.argv(None), ["/bin/sh", "-lc", "pi --mode rpc"]);
    }

    #[test]
    fn validation() {
        spec(Exec::Shell("cat".into())).validate().unwrap();
        SpawnSpec {
            kind: ProcessKind::Pty,
            ..spec(Exec::Shell("bash".into()))
        }
        .validate()
        .unwrap();
        let wrapper = Exec::Argv(vec!["nono".into(), "run".into(), "--".into(), "sh".into()]);
        spec(wrapper).validate().unwrap();

        let bad = [
            (spec(Exec::Argv(vec![])), "empty argv"),
            (spec(Exec::Shell(" ".into())), "empty shell"),
            (spec(Exec::Argv(vec!["a\0".into()])), "nul"),
            (
                SpawnSpec {
                    kind: ProcessKind::Unknown,
                    ..spec(Exec::Shell("x".into()))
                },
                "unknown kind",
            ),
            (
                SpawnSpec {
                    kind: ProcessKind::Pty,
                    plain_log: Some("/log.txt".into()),
                    ..spec(Exec::Shell("x".into()))
                },
                "pty plain log",
            ),
            (
                SpawnSpec {
                    cwd: "rel".into(),
                    ..spec(Exec::Shell("x".into()))
                },
                "cwd",
            ),
            (
                SpawnSpec {
                    timeout_ms: Some(0),
                    ..spec(Exec::Shell("x".into()))
                },
                "timeout",
            ),
            (
                SpawnSpec {
                    max_line_bytes: Some(MAX_LINE_BYTES + 1),
                    ..spec(Exec::Shell("x".into()))
                },
                "cap",
            ),
            (
                SpawnSpec {
                    plain_log: Some("log.txt".into()),
                    ..spec(Exec::Shell("x".into()))
                },
                "plain log",
            ),
            (
                SpawnSpec {
                    stdin: StdinMode::Unknown,
                    ..spec(Exec::Shell("x".into()))
                },
                "stdin",
            ),
        ];
        for (spec, what) in bad {
            assert!(spec.validate().is_err(), "{what}");
        }
    }

    #[test]
    fn env_file_format() {
        let vars = parse_env("# comment\n\nA=1\nB=x=y \nEMPTY=\n").unwrap();
        assert_eq!(
            vars,
            [
                ("A".into(), "1".into()),
                ("B".into(), "x=y ".into()),
                ("EMPTY".into(), String::new())
            ]
        );
        for bad in ["NOVALUE\n", "=x\n", "A B=x\n"] {
            assert!(parse_env(bad).is_err(), "{bad}");
        }
    }
}
