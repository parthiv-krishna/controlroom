//! Running host commands with a timeout, and resolving programs the way agents see them: through
//! the user's login shell (`$SHELL -lc`), which sets up `PATH` (spec §6.1).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use controlroom_sandbox::AbsPath;
use controlroom_sandbox::launch::shell_quote;

/// Login shells can be slow (nvm, asdf, direnv, ...).
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(20);

/// The user's login shell: `$SHELL` when it is an absolute path, else `/bin/sh`.
pub fn login_shell() -> AbsPath {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| AbsPath::new(&s).ok())
        .unwrap_or_else(|| AbsPath::new("/bin/sh").expect("static path"))
}

/// The captured result of a finished command.
#[derive(Debug, Clone)]
pub struct Output {
    /// Exit status 0.
    pub success: bool,
    /// Stdout, lossily decoded.
    pub stdout: String,
    /// Stderr, lossily decoded.
    pub stderr: String,
}

/// Why a command produced no [`Output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program doesn't exist.
    NotFound,
    /// It didn't finish in time and was killed.
    TimedOut,
    /// Any other failure.
    Other(String),
}

impl RunError {
    /// A one-line description naming `program`.
    pub fn describe(&self, program: &str) -> String {
        match self {
            RunError::NotFound => format!("`{program}` not found"),
            RunError::TimedOut => format!("`{program}` timed out"),
            RunError::Other(e) => format!("`{program}` failed: {e}"),
        }
    }
}

/// Runs `program` with stdin closed, capturing output, and kills it after `timeout`.
pub fn run(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<Output, RunError> {
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => RunError::NotFound,
            _ => RunError::Other(e.to_string()),
        })?;
    // Drain both pipes on threads so a chatty child can't block on a full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(RunError::Other(e.to_string())),
        }
    };
    Ok(Output {
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// The first word of a command line: the program it runs.
pub fn command_program(command: &str) -> Option<&str> {
    command.split_whitespace().next()
}

/// The resolved path printed by `command -v`: the last non-empty line, because shell startup
/// files may print to stdout.
pub fn parse_command_v(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_owned)
}

/// Resolves `program` through `shell -lc 'command -v <program>'`. `Ok(None)`: not on the login
/// shell's `PATH`.
pub fn resolve_program(shell: &AbsPath, program: &str) -> Result<Option<String>, RunError> {
    let script = format!("command -v {}", shell_quote(program));
    let out = run(shell.as_str(), &["-lc", &script], &[], LOGIN_SHELL_TIMEOUT)?;
    Ok(if out.success {
        parse_command_v(&out.stdout)
    } else {
        None
    })
}

/// Finds a program from a `sandbox.toml` setting (`nono`, `oauth_helper`): an absolute (or `~/`)
/// path as is, a program name through the login shell; then canonicalized (store paths, not
/// profile links).
pub fn find_program(shell: &AbsPath, setting: &str, home: &AbsPath) -> Result<AbsPath, String> {
    let path = if setting.starts_with('/') || setting.starts_with('~') {
        crate::dirs::expand_home(setting, home)
            .map_err(|e| e.to_string())?
            .to_string()
    } else {
        resolve_program(shell, setting)
            .map_err(|e| e.describe(shell.as_str()))?
            .ok_or_else(|| format!("`{setting}` not found by the login shell ({shell} -lc)"))?
    };
    let real = std::fs::canonicalize(&path).map_err(|e| format!("{path}: {e}"))?;
    AbsPath::from_path(&real).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines() {
        assert_eq!(command_program("  pi --mode rpc"), Some("pi"));
        assert_eq!(command_program(" "), None);
        assert_eq!(
            parse_command_v("Welcome!\n/home/alice/.local/bin/pi\n\n"),
            Some("/home/alice/.local/bin/pi".into())
        );
        assert_eq!(parse_command_v("\n"), None);
    }

    #[test]
    fn runs_and_times_out() {
        let out = run(
            "/bin/sh",
            &["-c", "echo out; echo err >&2; exit 3"],
            &[],
            LOGIN_SHELL_TIMEOUT,
        )
        .expect("sh runs");
        assert!(!out.success);
        assert_eq!((out.stdout.trim(), out.stderr.trim()), ("out", "err"));
        assert_eq!(
            run("/nonexistent/program", &[], &[], LOGIN_SHELL_TIMEOUT).unwrap_err(),
            RunError::NotFound
        );
        assert_eq!(
            run(
                "/bin/sh",
                &["-c", "exec sleep 5"],
                &[],
                Duration::from_millis(50)
            )
            .unwrap_err(),
            RunError::TimedOut
        );
    }

    #[test]
    fn resolves_through_a_shell() {
        let sh = AbsPath::new("/bin/sh").expect("path");
        assert!(resolve_program(&sh, "sh").expect("runs").is_some());
        assert_eq!(
            resolve_program(&sh, "controlroom-no-such-program").expect("runs"),
            None
        );
    }
}
