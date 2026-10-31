//! Target platforms, daemon versions, and the install decision.

use std::cmp::Ordering;
use std::fmt;

pub use semver::Version;

/// CPU architectures `controlroomd` is built for (static musl, Linux only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    /// `x86_64`
    X86_64,
    /// `aarch64`
    Aarch64,
}

impl Arch {
    /// Every supported architecture.
    pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    /// The `uname -m` style name.
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    /// Maps `uname -m` output. 32-bit ARM (`armv7l`, `armv8l`) and anything
    /// else is unsupported.
    pub fn from_uname_machine(machine: &str) -> Option<Arch> {
        match machine.trim() {
            "x86_64" | "amd64" => Some(Arch::X86_64),
            "aarch64" | "arm64" => Some(Arch::Aarch64),
            _ => None,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A machine controlroomd can't run on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported platform {os} {machine}: controlroomd runs on Linux x86_64 and aarch64 only")]
pub struct UnsupportedPlatform {
    /// `uname -s`
    pub os: String,
    /// `uname -m`
    pub machine: String,
}

/// Parses `uname -sm` output (`Linux x86_64`) into a supported [`Arch`].
pub fn parse_uname(uname_sm: &str) -> Result<Arch, UnsupportedPlatform> {
    let mut parts = uname_sm.split_whitespace();
    let os = parts.next().unwrap_or_default();
    let machine = parts.next().unwrap_or_default();
    let err = || UnsupportedPlatform {
        os: os.to_owned(),
        machine: machine.to_owned(),
    };
    if os != "Linux" {
        return Err(err());
    }
    Arch::from_uname_machine(machine).ok_or_else(err)
}

/// Extracts the version from `controlroomd --version` output
/// (`controlroomd 0.1.0`): the last word of the first line.
pub fn parse_version_output(output: &str) -> Option<Version> {
    let word = output.lines().next()?.split_whitespace().last()?;
    Version::parse(word.strip_prefix('v').unwrap_or(word)).ok()
}

/// What to do about the daemon on a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallDecision {
    /// No daemon is installed.
    Install,
    /// An older (or unrecognizable) daemon is installed.
    Upgrade {
        /// The installed version, or its raw `--version` output.
        from: String,
    },
    /// The installed daemon is the available version.
    UpToDate,
    /// The machine runs a newer daemon than this controller bundles (e.g.
    /// another controller upgraded it). Never downgrade: a newer daemon may
    /// own state in a newer format.
    RemoteNewer {
        /// The installed version.
        remote: Version,
    },
}

impl InstallDecision {
    /// Whether to install the available version.
    pub fn needs_install(&self) -> bool {
        matches!(
            self,
            InstallDecision::Install | InstallDecision::Upgrade { .. }
        )
    }
}

/// Decides from the installed daemon's `--version` output (`None` if absent).
///
/// Versions compare by semver precedence; build metadata is ignored, since
/// every daemon change bumps the version.
pub fn decide_install(
    installed_version_output: Option<&str>,
    available: &Version,
) -> InstallDecision {
    let Some(raw) = installed_version_output
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return InstallDecision::Install;
    };
    let Some(remote) = parse_version_output(raw) else {
        return InstallDecision::Upgrade {
            from: raw.to_owned(),
        };
    };
    match remote.cmp_precedence(available) {
        Ordering::Less => InstallDecision::Upgrade {
            from: remote.to_string(),
        },
        Ordering::Equal => InstallDecision::UpToDate,
        Ordering::Greater => InstallDecision::RemoteNewer { remote },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn uname_mapping() {
        assert_eq!(parse_uname("Linux x86_64\n"), Ok(Arch::X86_64));
        assert_eq!(parse_uname("Linux aarch64"), Ok(Arch::Aarch64));
        assert_eq!(parse_uname("Linux arm64"), Ok(Arch::Aarch64));
        for bad in [
            "Linux armv7l",
            "Linux i686",
            "Darwin arm64",
            "FreeBSD amd64",
            "",
        ] {
            assert!(parse_uname(bad).is_err(), "{bad}");
        }
        assert_eq!(
            parse_uname("Darwin arm64").unwrap_err(),
            UnsupportedPlatform {
                os: "Darwin".into(),
                machine: "arm64".into()
            }
        );
    }

    #[test]
    fn version_output() {
        assert_eq!(
            parse_version_output("controlroomd 0.4.2\n"),
            Some(v("0.4.2"))
        );
        assert_eq!(
            parse_version_output("controlroomd v1.0.0-rc.1"),
            Some(v("1.0.0-rc.1"))
        );
        assert_eq!(parse_version_output("sh: not found"), None);
        assert_eq!(parse_version_output(""), None);
    }

    #[test]
    fn install_decisions() {
        let avail = v("0.2.0");
        assert_eq!(decide_install(None, &avail), InstallDecision::Install);
        assert_eq!(
            decide_install(Some("  \n"), &avail),
            InstallDecision::Install
        );
        assert_eq!(
            decide_install(Some("controlroomd 0.1.9"), &avail),
            InstallDecision::Upgrade {
                from: "0.1.9".into()
            }
        );
        assert_eq!(
            decide_install(Some("controlroomd 0.2.0-rc.1"), &avail),
            InstallDecision::Upgrade {
                from: "0.2.0-rc.1".into()
            }
        );
        assert_eq!(
            decide_install(Some("controlroomd 0.2.0"), &avail),
            InstallDecision::UpToDate
        );
        assert_eq!(
            decide_install(Some("controlroomd 0.2.0+abc"), &avail),
            InstallDecision::UpToDate
        );
        assert_eq!(
            decide_install(Some("controlroomd 0.10.0"), &avail),
            InstallDecision::RemoteNewer {
                remote: v("0.10.0")
            }
        );
        let d = decide_install(Some("Segmentation fault"), &avail);
        assert!(d.needs_install(), "{d:?}");
        assert!(!InstallDecision::UpToDate.needs_install());
    }
}
