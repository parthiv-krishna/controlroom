//! Host checks for `controlroomd doctor`: Landlock ABI, nono availability, and nono's own
//! profile validation.

use std::process::Command;

use crate::launch::{NONO_ENV_PREFIX, NonoDirs};
use crate::paths::AbsPath;

/// The nono version controlroom is pinned to. Re-run the validation suite on every bump.
pub const PINNED_NONO_VERSION: &str = "0.80.0";

/// The kernel's Landlock support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landlock {
    /// Supported, with this ABI version (network filtering needs 4 or later).
    Abi(u32),
    /// Built in but disabled at boot (`lsm=` without landlock).
    Disabled,
    /// Not supported by this kernel.
    Unsupported,
}

/// Probes the Landlock ABI with `landlock_create_ruleset(NULL, 0, VERSION)`.
pub fn landlock() -> Landlock {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: with a null attribute pointer, size 0 and the VERSION flag the kernel only
    // returns the ABI version (or an error); no memory is read or written and no file
    // descriptor is created.
    #[allow(unsafe_code)]
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret >= 0 {
        return Landlock::Abi(u32::try_from(ret).unwrap_or(u32::MAX));
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EOPNOTSUPP) => Landlock::Disabled,
        _ => Landlock::Unsupported,
    }
}

/// Result of probing a nono binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonoStatus {
    /// nono runs and reports a version.
    Available {
        /// The reported version (e.g. `0.80.0`).
        version: String,
        /// Whether it equals [`PINNED_NONO_VERSION`].
        pinned: bool,
    },
    /// nono could not be run or printed something unexpected.
    Unavailable(String),
}

fn nono_command(nono: &AbsPath, dirs: &NonoDirs) -> Command {
    let mut cmd = Command::new(nono.as_path());
    cmd.env_clear()
        .envs(
            std::env::vars_os().filter(|(k, _)| !k.to_string_lossy().starts_with(NONO_ENV_PREFIX)),
        )
        .envs(dirs.env());
    cmd
}

/// Runs `nono --version` with controlroom's nono environment.
pub fn nono(nono: &AbsPath, dirs: &NonoDirs) -> NonoStatus {
    let out = match nono_command(nono, dirs).arg("--version").output() {
        Ok(o) => o,
        Err(e) => return NonoStatus::Unavailable(format!("{nono}: {e}")),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match text.trim().strip_prefix("nono ") {
        Some(version) if out.status.success() => NonoStatus::Available {
            version: version.to_owned(),
            pinned: version == PINNED_NONO_VERSION,
        },
        _ => NonoStatus::Unavailable(format!("unexpected `nono --version` output: {text:?}")),
    }
}

/// Runs `nono profile validate <profile>`; on failure returns nono's report.
pub fn validate_with_nono(
    nono: &AbsPath,
    dirs: &NonoDirs,
    profile: &AbsPath,
) -> Result<(), String> {
    let out = nono_command(nono, dirs)
        .args(["profile", "validate", profile.as_str()])
        .output()
        .map_err(|e| format!("{nono}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landlock_probe_does_not_fail() {
        // The result depends on the kernel; the probe must simply return.
        let _ = landlock();
    }

    #[test]
    fn missing_nono_is_unavailable() {
        let t = tempfile::tempdir().expect("tempdir");
        let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
        let status = nono(&root.join("nono"), &NonoDirs::under(&root));
        assert!(matches!(status, NonoStatus::Unavailable(_)));
    }
}
