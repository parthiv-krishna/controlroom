//! Probing a machine before install.

use super::platform::{
    Arch, InstallDecision, Version, decide_install, parse_uname, parse_version_output,
};
use super::{BootstrapError, REMOTE_CURRENT_BINARY, RemoteShell, field, run_script};

/// Prints `key=value` lines: `uname -sm`, `$HOME`, the installed daemon's
/// `--version` (empty if absent), and where `current` points.
const PROBE_SCRIPT: &str = r#"printf 'uname=%s\n' "$(uname -sm 2>/dev/null)"
printf 'home=%s\n' "$HOME"
d="$base/current/controlroomd"
if [ -x "$d" ]; then
  printf 'version=%s\n' "$(version_of "$d" 2>/dev/null | head -n 1)"
else
  printf 'version=\n'
fi
printf 'current=%s\n' "$(readlink "$base/current" 2>/dev/null)""#;

/// What a machine looks like before install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// The machine's architecture.
    pub arch: Arch,
    /// The remote `$HOME`, a POSIX path.
    pub home: String,
    /// First line of `controlroomd --version`, if a daemon is installed.
    pub daemon_version_output: Option<String>,
    /// Target of the `current` symlink (e.g. `versions/0.1.0`), if any.
    pub current_link: Option<String>,
}

impl ProbeResult {
    /// The installed daemon's version, if one is installed and recognizable.
    pub fn installed_version(&self) -> Option<Version> {
        self.daemon_version_output
            .as_deref()
            .and_then(parse_version_output)
    }

    /// What to do to bring this machine to `available`.
    pub fn decide(&self, available: &Version) -> InstallDecision {
        decide_install(self.daemon_version_output.as_deref(), available)
    }

    /// Absolute POSIX path of the stable daemon binary.
    pub fn current_binary_path(&self) -> String {
        format!(
            "{}/{REMOTE_CURRENT_BINARY}",
            self.home.trim_end_matches('/')
        )
    }
}

/// Parses the probe script's output; rejects unsupported platforms.
fn parse_probe_output(output: &str) -> Result<ProbeResult, BootstrapError> {
    let uname = field(output, "uname")
        .ok_or_else(|| BootstrapError::Parse(format!("probe: {output:?}")))?;
    let arch = parse_uname(uname)?;
    let home = field(output, "home")
        .filter(|h| h.starts_with('/'))
        .ok_or_else(|| BootstrapError::Parse("probe: $HOME is unset or not absolute".into()))?;
    let non_empty = |key| {
        field(output, key)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    Ok(ProbeResult {
        arch,
        home: home.to_owned(),
        daemon_version_output: non_empty("version"),
        current_link: non_empty("current"),
    })
}

/// Probes the machine: platform, `$HOME`, and the installed daemon.
pub async fn probe<S: RemoteShell>(shell: &S) -> Result<ProbeResult, BootstrapError> {
    let out = run_script(shell, "probe", PROBE_SCRIPT, b"").await?;
    parse_probe_output(&out.stdout_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fresh_machine() {
        let p = parse_probe_output("uname=Linux x86_64\nhome=/home/alice\nversion=\ncurrent=\n")
            .unwrap();
        assert_eq!(p.arch, Arch::X86_64);
        assert_eq!(p.home, "/home/alice");
        assert_eq!(p.daemon_version_output, None);
        assert_eq!(p.current_link, None);
        assert_eq!(p.decide(&Version::new(0, 1, 0)), InstallDecision::Install);
        assert_eq!(
            p.current_binary_path(),
            "/home/alice/.local/share/controlroom/current/controlroomd"
        );
    }

    #[test]
    fn parses_installed_daemon() {
        let p = parse_probe_output(
            "uname=Linux aarch64\nhome=/var/home/alice\nversion=controlroomd 0.1.0\ncurrent=versions/0.1.0\n",
        )
        .unwrap();
        assert_eq!(p.arch, Arch::Aarch64);
        assert_eq!(p.installed_version(), Some(Version::new(0, 1, 0)));
        assert_eq!(p.current_link.as_deref(), Some("versions/0.1.0"));
        assert_eq!(
            p.decide(&Version::new(0, 2, 0)),
            InstallDecision::Upgrade {
                from: "0.1.0".into()
            }
        );
    }

    #[test]
    fn rejects_unsupported_and_garbage() {
        let err = parse_probe_output("uname=Darwin arm64\nhome=/Users/alice\n").unwrap_err();
        assert!(
            matches!(err, BootstrapError::UnsupportedPlatform(_)),
            "{err}"
        );
        let err = parse_probe_output("uname=Linux riscv64\nhome=/home/alice\n").unwrap_err();
        assert!(err.to_string().contains("riscv64"), "{err}");
        assert!(parse_probe_output("fish: unknown command").is_err());
        assert!(parse_probe_output("uname=Linux x86_64\nhome=\n").is_err());
    }
}
