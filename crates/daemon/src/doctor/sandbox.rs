//! Sandbox prerequisites (spec §11.5): which nono sandboxes workspaces (the one installed with
//! controlroomd, or `sandbox.toml`'s), its version against the validated 0.80.0 and its build
//! (the first digits of its SHA-256, as in the installed `nono/<version>-<sha>/` directory, which
//! tells a patched rebuild of a version from the original), and the kernel's Landlock ABI; plus
//! the sftp placeholder for future file browsing.

use controlroom_protocol::host::{CheckStatus, DoctorCheck};
use controlroom_sandbox::AbsPath;
use controlroom_sandbox::launch::NonoDirs;
use controlroom_sandbox::probe::{self, Landlock, NonoStatus, PINNED_NONO_VERSION};

use super::{check, unknown};
use crate::dirs::Dirs;
use crate::host::fsops::sha256_hex;
use crate::nono::{Nono, NonoMissing, NonoOrigin, locate};

/// Landlock ABI that filters TCP (nono's network confinement needs it).
const NETWORK_ABI: u32 = 4;

/// Locates nono (see [`crate::nono`]) for the daemon at `exe` and checks it runs and its
/// version against the validated one.
pub fn nono(dirs: &Dirs, exe: &AbsPath, setting: Option<&str>) -> DoctorCheck {
    match locate(setting, exe, dirs.home()) {
        Ok(nono) => {
            let status = probe::nono(&nono.path, &NonoDirs::under(dirs.state()));
            let build = std::fs::read(nono.path.as_path())
                .ok()
                .map(|binary| sha256_hex(&binary)[..BUILD_DIGITS].to_owned());
            nono_status(&nono, status, build.as_deref())
        }
        Err(missing) => nono_missing(&missing),
    }
}

fn nono_missing(missing: &NonoMissing) -> DoctorCheck {
    let fix = match missing.origin {
        NonoOrigin::Bundled => "reinstall controlroomd from controlroom",
        NonoOrigin::Configured => "fix or remove `nono` in sandbox.toml (to use the bundled one)",
    };
    check(
        CheckStatus::Fail,
        "missing",
        format!("{missing}; sandboxed sessions can't start: {fix}"),
    )
}

/// Hex digits of the SHA-256 shown for the nono build.
const BUILD_DIGITS: usize = 12;

/// The check for `nono` as `status` found it; `build` is its short SHA-256, if readable.
fn nono_status(nono: &Nono, status: NonoStatus, build: Option<&str>) -> DoctorCheck {
    let Nono { path, origin } = nono;
    let shown = |version: &str| match build {
        Some(build) => format!("{version} ({build})"),
        None => version.to_owned(),
    };
    match status {
        NonoStatus::Available {
            version,
            pinned: true,
        } => check(
            CheckStatus::Ok,
            &shown(&version),
            format!("{origin}: {path}"),
        ),
        NonoStatus::Available { version, .. } => check(
            CheckStatus::Warn,
            &shown(&version),
            format!(
                "{origin}: {path}; controlroom's sandbox profiles are validated against nono {PINNED_NONO_VERSION}"
            ),
        ),
        NonoStatus::Unavailable(why) => check(
            CheckStatus::Fail,
            "unusable",
            format!("{origin}: {why}; sandboxed sessions can't start"),
        ),
    }
}

/// The kernel's Landlock ABI, and its check.
pub fn landlock() -> (Option<u32>, DoctorCheck) {
    landlock_status(probe::landlock())
}

fn landlock_status(landlock: Landlock) -> (Option<u32>, DoctorCheck) {
    match landlock {
        Landlock::Abi(abi) if abi >= NETWORK_ABI => (
            Some(abi),
            check(
                CheckStatus::Ok,
                &abi.to_string(),
                "filesystem and network confinement",
            ),
        ),
        Landlock::Abi(abi) => (
            Some(abi),
            check(
                CheckStatus::Warn,
                &abi.to_string(),
                format!("ABI {NETWORK_ABI} or later is needed to confine TCP; upgrade the kernel"),
            ),
        ),
        Landlock::Disabled => (
            None,
            check(
                CheckStatus::Fail,
                "disabled",
                "Landlock is built in but disabled; add it to the `lsm=` boot parameter",
            ),
        ),
        Landlock::Unsupported => (
            None,
            check(
                CheckStatus::Fail,
                "unsupported",
                "this kernel has no Landlock; sandboxed sessions can't start",
            ),
        ),
    }
}

/// Placeholder for file browsing (future, spec §3): reads the `Subsystem sftp` line of
/// OpenSSH's server config, when readable. (Match blocks and included files are not followed.)
pub fn sftp() -> DoctorCheck {
    match std::fs::read_to_string("/etc/ssh/sshd_config") {
        Ok(config) => sftp_status(&config, |p| std::path::Path::new(p).exists()),
        Err(e) => unknown(format!("cannot read /etc/ssh/sshd_config: {e}")),
    }
}

fn sftp_status(config: &str, exists: impl Fn(&str) -> bool) -> DoctorCheck {
    let server = config.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let keyword = words.next()?;
        let name = words.next()?;
        (keyword.eq_ignore_ascii_case("subsystem") && name == "sftp")
            .then(|| words.next())
            .flatten()
    });
    match server {
        Some("internal-sftp") => check(CheckStatus::Ok, "yes", "internal-sftp"),
        Some(path) if exists(path) => check(CheckStatus::Ok, "yes", path),
        Some(path) => check(CheckStatus::Warn, "no", format!("{path} does not exist")),
        None => check(
            CheckStatus::Warn,
            "no",
            "no `Subsystem sftp` in /etc/ssh/sshd_config; file browsing (future) needs it",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nono_statuses() {
        let nono = |origin| Nono {
            path: AbsPath::new(
                "/home/alice/.local/share/controlroom/nono/0.80.0-0123456789ab/nono",
            )
            .unwrap(),
            origin,
        };
        let ok = nono_status(
            &nono(NonoOrigin::Bundled),
            NonoStatus::Available {
                version: "0.80.0".into(),
                pinned: true,
            },
            Some("0123456789ab"),
        );
        assert_eq!(
            (ok.status, ok.value.as_deref()),
            (CheckStatus::Ok, Some("0.80.0 (0123456789ab)"))
        );
        assert_eq!(
            ok.detail,
            "bundled: /home/alice/.local/share/controlroom/nono/0.80.0-0123456789ab/nono"
        );
        // Another version still runs, with a warning.
        let old = nono_status(
            &nono(NonoOrigin::Configured),
            NonoStatus::Available {
                version: "0.79.0".into(),
                pinned: false,
            },
            None,
        );
        assert_eq!(
            (old.status, old.value.as_deref()),
            (CheckStatus::Warn, Some("0.79.0"))
        );
        assert!(old.detail.starts_with("configured in sandbox.toml: "));
        assert!(old.detail.contains("validated against nono 0.80.0"));
        let broken = nono_status(
            &nono(NonoOrigin::Bundled),
            NonoStatus::Unavailable("exec failed".into()),
            None,
        );
        assert_eq!(broken.status, CheckStatus::Fail);
    }

    #[test]
    fn missing_nono_advice() {
        let bundled = nono_missing(&NonoMissing {
            origin: NonoOrigin::Bundled,
            detail: "the bundled nono … is missing".into(),
        });
        assert_eq!(
            (bundled.status, bundled.value.as_deref()),
            (CheckStatus::Fail, Some("missing"))
        );
        assert!(bundled.detail.contains("reinstall controlroomd"));
        assert!(!bundled.detail.contains("install nono"));
        let configured = nono_missing(&NonoMissing {
            origin: NonoOrigin::Configured,
            detail: "`nono` not found by the login shell".into(),
        });
        assert!(configured.detail.contains("sandbox.toml"));
    }

    #[test]
    fn sftp_subsystem() {
        let exists = |p: &str| p == "/usr/lib/openssh/sftp-server";
        let status = |config: &str| sftp_status(config, exists).status;
        assert_eq!(
            status("# x\nSubsystem sftp /usr/lib/openssh/sftp-server\n"),
            CheckStatus::Ok
        );
        assert_eq!(
            status("subsystem  sftp  internal-sftp -u 077"),
            CheckStatus::Ok
        );
        assert_eq!(
            status("Subsystem sftp /missing/sftp-server"),
            CheckStatus::Warn
        );
        assert_eq!(status("Port 22\n"), CheckStatus::Warn);
    }

    #[test]
    fn landlock_statuses() {
        assert_eq!(landlock_status(Landlock::Abi(6)).1.status, CheckStatus::Ok);
        let (abi, old) = landlock_status(Landlock::Abi(3));
        assert_eq!((abi, old.status), (Some(3), CheckStatus::Warn));
        assert_eq!(landlock_status(Landlock::Disabled).0, None);
        assert_eq!(
            landlock_status(Landlock::Unsupported).1.status,
            CheckStatus::Fail
        );
    }
}
