//! Installing `controlroomd`, and rolling an install back.
//!
//! The binary is streamed over an exec channel into a temp file inside
//! `versions/<ver>/` and checked, in order:
//! 1. the byte count (`wc -c`; catches truncated writes such as a full disk);
//! 2. SHA-256 with the first tool found: `sha256sum`, `shasum -a 256`,
//!    `openssl dgst -sha256` (skipped if none exists; SSH's MAC still
//!    protects the transfer);
//! 3. a verify-run: the new binary must run on this machine and print the
//!    expected version for `--version`.
//!
//! Only then is it moved to `versions/<ver>/controlroomd` and `current`
//! swapped to it atomically (`ln -s` to a temp name, then `mv -T`). The
//! previous `current` target is returned so the caller can [`rollback`] if
//! the new daemon doesn't come up. Nothing needs root.

use sha2::{Digest, Sha256};

use super::binaries::DaemonBinarySource;
use super::platform::{Arch, InstallDecision, Version};
use super::probe::ProbeResult;
use super::{BootstrapError, RemoteShell, field, run_script};
use crate::shell::{quote, sh_c};
use crate::ssh::ExitStatus;

/// Defines `swap_current <target>`: points `current` at `<target>` atomically.
const SWAP_CURRENT: &str = r#"swap_current() {
  link="$base/.current-$$"
  rm -f "$link"
  ln -s "$1" "$link"
  if ! mv -T "$link" "$base/current" 2>/dev/null; then
    # No `mv -T` (e.g. an old busybox): replace the link non-atomically.
    rm -f "$base/current"
    mv "$link" "$base/current"
  fi
}
"#;

/// Exit codes the install script uses for verification failures.
const EXIT_SIZE: u32 = 3;
const EXIT_HASH: u32 = 4;
const EXIT_VERIFY_RUN: u32 = 5;

/// A daemon version installed and activated by [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The installed version.
    pub version: Version,
    /// The machine's architecture.
    pub arch: Arch,
    /// Absolute POSIX path of the installed, immutable binary.
    pub path: String,
    /// The hash tool that verified the upload (`sha256sum`, `shasum`,
    /// `openssl`), or `none` if the machine has none.
    pub hashed_with: String,
    /// The `current` target before the swap (e.g. `versions/0.1.0`), for
    /// [`rollback`]; `None` on a fresh install.
    pub previous: Option<String>,
}

impl Installed {
    /// The `current` target this install set, e.g. `versions/0.2.0`.
    pub fn target(&self) -> String {
        format!("versions/{}", self.version)
    }
}

fn install_script(version: &Version, sha256_hex: &str, size: usize) -> String {
    format!(
        r#"set -eu
{SWAP_CURRENT}ver={ver}
want={sha}
size={size}
dir="$base/versions/$ver"
mkdir -p "$dir"
tmp="$dir/.upload-$$"
trap 'rm -f "$tmp" "$base/.current-$$"; rmdir "$dir" 2>/dev/null || true' EXIT
cat > "$tmp"
got_size=$(wc -c < "$tmp" | tr -d ' ')
if [ "$got_size" != "$size" ]; then
  echo "size mismatch: expected $size bytes, got $got_size" >&2
  exit {EXIT_SIZE}
fi
how=none
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp" | cut -d ' ' -f 1); how=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  got=$(shasum -a 256 "$tmp" | cut -d ' ' -f 1); how=shasum
elif command -v openssl >/dev/null 2>&1; then
  got=$(openssl dgst -sha256 -r "$tmp" | cut -d ' ' -f 1); how=openssl
fi
if [ "$how" != none ] && [ "$got" != "$want" ]; then
  echo "sha256 mismatch ($how): expected $want, got $got" >&2
  exit {EXIT_HASH}
fi
chmod 755 "$tmp"
if ! out=$(version_of "$tmp" 2>&1); then
  echo "verify-run: --version failed: $out" >&2
  exit {EXIT_VERIFY_RUN}
fi
first=$(printf '%s\n' "$out" | head -n 1)
case "$first" in
  *" $ver"|*" v$ver") ;;
  *) echo "verify-run: expected version $ver, got: $first" >&2; exit {EXIT_VERIFY_RUN} ;;
esac
mv -f "$tmp" "$dir/controlroomd"
previous=$(readlink "$base/current" 2>/dev/null || true)
swap_current "versions/$ver"
printf 'path=%s\nhashed=%s\nprevious=%s\n' "$dir/controlroomd" "$how" "$previous""#,
        ver = quote(&version.to_string()),
        sha = quote(sha256_hex),
    )
}

/// Uploads, verifies and activates the daemon from `source` on the machine
/// `probe` describes.
///
/// Refuses to downgrade a newer installed daemon. If verification fails,
/// `current` is untouched and the error is [`BootstrapError::Verification`].
pub async fn install<S: RemoteShell, B: DaemonBinarySource + ?Sized>(
    shell: &S,
    source: &B,
    probe: &ProbeResult,
) -> Result<Installed, BootstrapError> {
    let version = source.version().clone();
    if let InstallDecision::RemoteNewer { remote } = probe.decide(&version) {
        return Err(BootstrapError::WouldDowngrade {
            installed: remote,
            available: version,
        });
    }
    let arch = probe.arch;
    let binary = source
        .binary(arch)
        .map_err(|source| BootstrapError::MissingBinary { arch, source })?;
    let sha = data_encoding::HEXLOWER.encode(&Sha256::digest(&binary));
    let script = install_script(&version, &sha, binary.len());
    let out = match run_script(shell, "install", &script, &binary).await {
        Err(BootstrapError::CommandFailed {
            status: ExitStatus::Code(EXIT_SIZE | EXIT_HASH | EXIT_VERIFY_RUN),
            stderr,
            ..
        }) => return Err(BootstrapError::Verification(stderr)),
        other => other?,
    };
    let stdout = out.stdout_str();
    let (Some(path), Some(hashed_with)) = (field(&stdout, "path"), field(&stdout, "hashed")) else {
        return Err(BootstrapError::Parse(format!("install: {stdout:?}")));
    };
    Ok(Installed {
        version,
        arch,
        path: path.to_owned(),
        hashed_with: hashed_with.to_owned(),
        previous: field(&stdout, "previous")
            .filter(|p| !p.is_empty())
            .map(str::to_owned),
    })
}

/// Points `current` back at `installed.previous`, or removes it if there was
/// no previous daemon. Refuses if `current` no longer points at the
/// installed version (someone else changed it since).
///
/// Returns the restored target. The installed binary stays in `versions/`.
pub async fn rollback<S: RemoteShell>(
    shell: &S,
    installed: &Installed,
) -> Result<Option<String>, BootstrapError> {
    let script = format!(
        r#"set -eu
{SWAP_CURRENT}expect={expect}
previous={previous}
cur=$(readlink "$base/current" 2>/dev/null || true)
if [ "$cur" != "$expect" ]; then
  echo "current points at '$cur', not '$expect'; not rolling back" >&2
  exit 6
fi
if [ -n "$previous" ]; then swap_current "$previous"; else rm -f "$base/current"; fi"#,
        expect = quote(&installed.target()),
        previous = quote(installed.previous.as_deref().unwrap_or_default()),
    );
    run_script(shell, "rollback", &script, b"").await?;
    Ok(installed.previous.clone())
}

/// The target of the `current` link (e.g. `versions/0.1.0`), if it exists.
pub async fn read_current_link<S: RemoteShell>(
    shell: &S,
) -> Result<Option<String>, BootstrapError> {
    let out = shell
        .run(
            &sh_c(r#"readlink "$HOME/.local/share/controlroom/current" || true"#),
            b"",
        )
        .await?;
    let target = out.stdout_str().trim().to_owned();
    Ok((!target.is_empty()).then_some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_quotes_inputs() {
        let s = install_script(&Version::parse("1.2.3-rc.1").unwrap(), "abc", 42);
        assert!(s.contains("ver='1.2.3-rc.1'\n"));
        assert!(s.contains("want='abc'\n"));
        assert!(s.contains("size=42\n"));
    }
}
