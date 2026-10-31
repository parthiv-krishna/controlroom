//! Shared test helpers: the client-core harness (feature `test-support`),
//! OpenSSH test keys, fake daemon binaries, and process cleanup.

#![allow(dead_code)]

pub mod test_keys;

use std::path::{Path, PathBuf};

pub use controlroom_client_core::test_support::*;

/// How a fake daemon behaves.
pub struct Fake<'a> {
    /// The version it was built as.
    pub version: &'a str,
    /// What `--version` prints; `None` makes it fail.
    pub version_output: Option<String>,
    /// Whether `hello` succeeds (standing in for the RPC handshake).
    pub hello_ok: bool,
    /// Whether `serve` exits on SIGTERM.
    pub obeys_term: bool,
}

impl<'a> Fake<'a> {
    /// A well-behaved daemon reporting `version`.
    pub fn good(version: &'a str) -> Self {
        Self {
            version,
            version_output: Some(format!("controlroomd {version}")),
            hello_ok: true,
            obeys_term: true,
        }
    }

    /// The fake as a bash script. `serve` and `supervise` re-exec bash with
    /// `argv[0]` set to the daemon's path, so their command lines read
    /// `<path> serve` like the real daemon's, and they touch
    /// `$HOME/<subcommand>.ready` once their TERM trap is installed.
    pub fn binary(&self) -> Vec<u8> {
        let version = match &self.version_output {
            Some(out) => format!("echo '{out}'"),
            None => "echo 'Illegal instruction' >&2; exit 132".into(),
        };
        let hello = if self.hello_ok {
            format!("echo 'hello from {}'", self.version)
        } else {
            "echo 'incompatible' >&2; exit 1".into()
        };
        let trap = if self.obeys_term {
            "trap 'exit 0' TERM"
        } else {
            "trap '' TERM"
        };
        format!(
            r#"#!{bash}
case "$1" in
  --version) {version} ;;
  hello) {hello} ;;
  serve|supervise)
    d="$HOME/.fake-$1-$$"
    mkdir -p "$d"
    printf '%s\n' "{trap}" "touch \"\$HOME/$1.ready\"" 'while :; do sleep 0.05; done' > "$d/$1"
    cd "$d" && exec -a "$0" "$BASH" "$1" ;;
  *) echo "unknown command $1" >&2; exit 2 ;;
esac
"#,
            bash = bash().display(),
        )
        .into_bytes()
    }
}

/// A well-behaved fake daemon binary for `version`.
pub fn fake_daemon(version: &str) -> Vec<u8> {
    Fake::good(version).binary()
}

/// The absolute path of `name` on `PATH`.
pub fn which(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not on PATH"))
}

fn bash() -> PathBuf {
    which("bash")
}

/// A shell command that starts the daemon's `<subcommand>` in the background
/// from `path` (relative to `$HOME`) and returns once it is ready.
pub fn start_daemon_command(path: &str, subcommand: &str) -> String {
    format!(
        r#"sh -c 'rm -f "$HOME/{subcommand}.ready"; "$HOME/{path}" {subcommand} >/dev/null 2>&1 &
while [ ! -e "$HOME/{subcommand}.ready" ]; do sleep 0.01; done'"#
    )
}

/// Kills (SIGKILL) every process whose command line mentions `home` when
/// dropped, so fake daemons never outlive a test, even a failing one.
pub struct Reaper(pub PathBuf);

impl Reaper {
    /// The pids of live processes whose command line mentions the home.
    pub fn pids(&self) -> Vec<u32> {
        let needle = self.0.to_string_lossy().into_owned();
        std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| {
                let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                let state =
                    std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                let zombie = state
                    .rsplit(") ")
                    .next()
                    .is_some_and(|s| s.starts_with('Z'));
                !zombie && String::from_utf8_lossy(&cmdline).contains(&needle)
            })
            .collect()
    }

    /// Whether a process with this exact command line (NULs as spaces) is alive.
    pub fn running(&self, cmdline_prefix: &str) -> bool {
        self.pids().iter().any(|pid| {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            String::from_utf8_lossy(&cmdline)
                .replace('\0', " ")
                .starts_with(cmdline_prefix)
        })
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in self.pids() {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}

/// `<home>/.local/share/controlroom`.
pub fn base(home: &Path) -> PathBuf {
    home.join(".local/share/controlroom")
}
