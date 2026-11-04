//! Sandbox prerequisites (spec §11.5): nono, pinned to 0.80.0, and the kernel's Landlock ABI.

use controlroom_protocol::host::{CheckStatus, DoctorCheck};
use controlroom_sandbox::AbsPath;
use controlroom_sandbox::launch::NonoDirs;
use controlroom_sandbox::probe::{self, Landlock, NonoStatus, PINNED_NONO_VERSION};

use super::check;
use crate::dirs::Dirs;
use crate::launch::find_program;

/// Landlock ABI that filters TCP (nono's network confinement needs it).
const NETWORK_ABI: u32 = 4;

/// Finds nono (see [`find_program`]) and checks its version against the pin.
pub fn nono(dirs: &Dirs, shell: &AbsPath, setting: &str) -> DoctorCheck {
    let path = match find_program(shell, setting) {
        Ok(path) => path,
        Err(e) => {
            return check(
                CheckStatus::Fail,
                "missing",
                format!("{e}; sandboxed sessions can't start"),
            );
        }
    };
    nono_status(&path, probe::nono(&path, &NonoDirs::under(dirs.state())))
}

fn nono_status(path: &AbsPath, status: NonoStatus) -> DoctorCheck {
    match status {
        NonoStatus::Available {
            version,
            pinned: true,
        } => check(CheckStatus::Ok, &version, format!("{path}")),
        NonoStatus::Available { version, .. } => check(
            CheckStatus::Fail,
            &version,
            format!(
                "{path}: controlroom's sandbox profiles are validated against nono {PINNED_NONO_VERSION}; install that version"
            ),
        ),
        NonoStatus::Unavailable(why) => check(
            CheckStatus::Fail,
            "unusable",
            format!("{why}; sandboxed sessions can't start"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nono_statuses() {
        let path = AbsPath::new("/nix/store/aaaa-nono-0.80.0/bin/nono").unwrap();
        let ok = nono_status(
            &path,
            NonoStatus::Available {
                version: "0.80.0".into(),
                pinned: true,
            },
        );
        assert_eq!(
            (ok.status, ok.value.as_deref()),
            (CheckStatus::Ok, Some("0.80.0"))
        );
        let old = nono_status(
            &path,
            NonoStatus::Available {
                version: "0.79.0".into(),
                pinned: false,
            },
        );
        assert_eq!(old.status, CheckStatus::Fail);
        assert!(old.detail.contains("0.80.0"));
        let broken = nono_status(&path, NonoStatus::Unavailable("exec failed".into()));
        assert_eq!(broken.status, CheckStatus::Fail);
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
