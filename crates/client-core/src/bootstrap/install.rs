//! Installing `controlroomd` with its nono, and rolling an install back.
//!
//! nono goes first, into `nono/<nono version>-<sha>/`, keyed by its content (the first 12 hex
//! digits of the bundled binary's SHA-256): a machine that already has that build (checked by
//! running it and, with a hash tool, by its hash) gets nothing uploaded, so a daemon upgrade
//! with the same nono uploads only the daemon, while a rebuilt nono of the same version (a new
//! patch) is uploaded next to the old one. Then the daemon goes into `versions/<version>/`, next
//! to a `nono` link to its nono, and `current` is swapped to it. Old nono builds stay for the
//! daemon versions that link them, until [`super::prune_versions`] removes them.
//!
//! Each binary is streamed over an exec channel into a temp file in its directory and
//! checked, in order:
//! 1. the byte count (`wc -c`; catches truncated writes such as a full disk);
//! 2. SHA-256 with the first tool found: `sha256sum`, `shasum -a 256`,
//!    `openssl dgst -sha256` (skipped if none exists; SSH's MAC still
//!    protects the transfer);
//! 3. a verify-run: the new binary must run on this machine and print the
//!    expected version for `--version`.
//!
//! Only then is it renamed into place, so a failed upload never leaves a partial binary
//! where anything runs it. `current` is swapped atomically (`ln -s` to a temp name, then
//! `mv -T`). The previous `current` target is returned so the caller can [`rollback`] if
//! the new daemon doesn't come up; that version's directory has its own `nono` link, so a
//! rollback restores its nono too. Nothing needs root.

use std::fmt;

use sha2::{Digest, Sha256};

use super::binaries::{DaemonBinarySource, Program};
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

/// Exit codes the upload scripts use for verification failures.
const EXIT_SIZE: u32 = 3;
const EXIT_HASH: u32 = 4;
const EXIT_VERIFY_RUN: u32 = 5;

/// A binary [`install`] is about to upload, reported to its progress callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallStep<'a> {
    /// What is uploaded.
    pub program: Program,
    /// Its version.
    pub version: &'a Version,
    /// The machine's architecture.
    pub arch: Arch,
}

impl fmt::Display for InstallStep<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "installing {} {} ({})",
            self.program, self.version, self.arch
        )
    }
}

/// The nono an installed daemon uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledNono {
    /// Its version.
    pub version: Version,
    /// The SHA-256 of the binary (hex).
    pub sha256: String,
    /// Absolute POSIX path of the binary, `<base>/nono/<version>-<sha>/nono`.
    pub path: String,
    /// Whether this install uploaded it (it wasn't on the machine yet).
    pub uploaded: bool,
}

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
    /// The daemon's nono.
    pub nono: InstalledNono,
}

impl Installed {
    /// The `current` target this install set, e.g. `versions/0.2.0`.
    pub fn target(&self) -> String {
        format!("versions/{}", self.version)
    }
}

/// Hex digits of a nono's SHA-256 in its directory name.
const NONO_KEY_DIGITS: usize = 12;

/// The directory of a nono build, relative to the data dir: `nono/<version>-<sha prefix>`.
fn nono_dir(version: &Version, sha256_hex: &str) -> String {
    let key = &sha256_hex[..NONO_KEY_DIGITS.min(sha256_hex.len())];
    format!("nono/{version}-{key}")
}

/// A script that streams stdin into a temp file in `$base/<dir>`, verifies it (size, hash,
/// verify-run) and then runs `finish`, which renames `$tmp` into place. On any failure the
/// temp file (and the directory, if empty) is removed.
fn upload_script(
    dir: &str,
    version: &Version,
    sha256_hex: &str,
    size: usize,
    finish: &str,
) -> String {
    format!(
        r#"set -eu
{SWAP_CURRENT}ver={ver}
want={sha}
size={size}
dir="$base/"{dir}
mkdir -p "$dir"
tmp="$dir/.upload-$$"
trap 'rm -f "$tmp" "$dir/.nono-$$" "$base/.current-$$"; rmdir "$dir" 2>/dev/null || true' EXIT
cat > "$tmp"
got_size=$(wc -c < "$tmp" | tr -d ' ')
if [ "$got_size" != "$size" ]; then
  echo "size mismatch: expected $size bytes, got $got_size" >&2
  exit {EXIT_SIZE}
fi
sha256_of "$tmp"
if [ "$how" != none ] && [ "$got" != "$want" ]; then
  echo "sha256 mismatch ($how): expected $want, got $got" >&2
  exit {EXIT_HASH}
fi
chmod 755 "$tmp"
if ! out=$(version_of "$tmp" 2>&1); then
  echo "verify-run: --version failed: $out" >&2
  exit {EXIT_VERIFY_RUN}
fi
if ! reports_version "$out" "$ver"; then
  echo "verify-run: expected version $ver, got: $(printf '%s\n' "$out" | head -n 1)" >&2
  exit {EXIT_VERIFY_RUN}
fi
{finish}"#,
        ver = quote(&version.to_string()),
        sha = quote(sha256_hex),
        dir = quote(dir),
    )
}

/// Uploads `program`'s binary with `script` and maps verification failures.
async fn upload<S: RemoteShell>(
    shell: &S,
    program: Program,
    script: &str,
    binary: &[u8],
) -> Result<String, BootstrapError> {
    let what = match program {
        Program::Controlroomd => "install controlroomd",
        Program::Nono => "install nono",
    };
    match run_script(shell, what, script, binary).await {
        Ok(out) => Ok(out.stdout_str()),
        Err(BootstrapError::CommandFailed {
            status: ExitStatus::Code(EXIT_SIZE | EXIT_HASH | EXIT_VERIFY_RUN),
            stderr,
            ..
        }) => Err(BootstrapError::Verification {
            program,
            detail: stderr,
        }),
        Err(e) => Err(e),
    }
}

/// The binary of `program` for `arch` and its SHA-256.
fn binary<'a, B: DaemonBinarySource + ?Sized>(
    source: &'a B,
    program: Program,
    arch: Arch,
) -> Result<(std::borrow::Cow<'a, [u8]>, String), BootstrapError> {
    let binary = source
        .binary(program, arch)
        .map_err(|source| BootstrapError::MissingBinary {
            program,
            arch,
            source,
        })?;
    let sha = data_encoding::HEXLOWER.encode(&Sha256::digest(&binary));
    Ok((binary, sha))
}

/// Makes sure the machine has the source's nono build: keeps an installed copy that runs,
/// reports the version and (with a hash tool) has the same SHA-256, else uploads and verifies
/// it.
async fn ensure_nono<S: RemoteShell, B: DaemonBinarySource + ?Sized>(
    shell: &S,
    source: &B,
    arch: Arch,
    progress: &(dyn Fn(InstallStep<'_>) + Sync),
) -> Result<InstalledNono, BootstrapError> {
    let version = source.version(Program::Nono).clone();
    // The content key: the binary is read (and hashed) even when the machine has it.
    let (bytes, sha) = binary(source, Program::Nono, arch)?;
    let dir = nono_dir(&version, &sha);
    let check = format!(
        r#"f="$base/"{dir}/nono
if [ -x "$f" ] && out=$(version_of "$f" 2>/dev/null) && reports_version "$out" {ver}; then
  sha256_of "$f"
  if [ "$how" = none ] || [ "$got" = {sha} ]; then printf 'present=%s\n' "$f"; fi
fi"#,
        dir = quote(&dir),
        ver = quote(&version.to_string()),
        sha = quote(&sha),
    );
    let out = run_script(shell, "check nono", &check, b"").await?;
    if let Some(path) = field(&out.stdout_str(), "present") {
        return Ok(InstalledNono {
            version,
            sha256: sha,
            path: path.to_owned(),
            uploaded: false,
        });
    }
    progress(InstallStep {
        program: Program::Nono,
        version: &version,
        arch,
    });
    let script = upload_script(
        &dir,
        &version,
        &sha,
        bytes.len(),
        r#"mv -f "$tmp" "$dir/nono"
printf 'path=%s\n' "$dir/nono""#,
    );
    let stdout = upload(shell, Program::Nono, &script, &bytes).await?;
    let path = field(&stdout, "path")
        .ok_or_else(|| BootstrapError::Parse(format!("install nono: {stdout:?}")))?;
    Ok(InstalledNono {
        version,
        sha256: sha,
        path: path.to_owned(),
        uploaded: true,
    })
}

/// Uploads, verifies and activates the daemon from `source`, with its nono, on the machine
/// `probe` describes. `progress` hears about each upload as it starts.
///
/// Refuses to downgrade a newer installed daemon. If verification fails, `current` is
/// untouched and the error is [`BootstrapError::Verification`].
pub async fn install<S: RemoteShell, B: DaemonBinarySource + ?Sized>(
    shell: &S,
    source: &B,
    probe: &ProbeResult,
    progress: &(dyn Fn(InstallStep<'_>) + Sync),
) -> Result<Installed, BootstrapError> {
    let version = source.version(Program::Controlroomd).clone();
    if let InstallDecision::RemoteNewer { remote } = probe.decide(&version) {
        return Err(BootstrapError::WouldDowngrade {
            installed: remote,
            available: version,
        });
    }
    let arch = probe.arch;
    let (daemon, sha) = binary(source, Program::Controlroomd, arch)?;
    let nono = ensure_nono(shell, source, arch, progress).await?;
    progress(InstallStep {
        program: Program::Controlroomd,
        version: &version,
        arch,
    });
    let finish = format!(
        r#"nono={nono}/nono
if [ ! -x "$base/$nono" ]; then
  echo "$base/$nono is missing" >&2
  exit 7
fi
ln -s "../../$nono" "$dir/.nono-$$"
mv -f "$dir/.nono-$$" "$dir/nono"
mv -f "$tmp" "$dir/controlroomd"
previous=$(readlink "$base/current" 2>/dev/null || true)
swap_current "versions/$ver"
printf 'path=%s\nhashed=%s\nprevious=%s\n' "$dir/controlroomd" "$how" "$previous""#,
        nono = quote(&nono_dir(&nono.version, &nono.sha256)),
    );
    let script = upload_script(
        &format!("versions/{version}"),
        &version,
        &sha,
        daemon.len(),
        &finish,
    );
    let stdout = upload(shell, Program::Controlroomd, &script, &daemon).await?;
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
        nono,
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
        let v = Version::parse("1.2.3-rc.1").unwrap();
        let s = upload_script("versions/1.2.3-rc.1", &v, "abc", 42, "true");
        assert!(s.contains("ver='1.2.3-rc.1'\n"));
        assert!(s.contains("want='abc'\n"));
        assert!(s.contains("size=42\n"));
        assert!(s.contains("dir=\"$base/\"'versions/1.2.3-rc.1'\n"));
        assert!(s.ends_with("\ntrue"));
    }

    #[test]
    fn nono_dirs_are_keyed_by_version_and_content() {
        let v = Version::new(0, 80, 0);
        let sha = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(nono_dir(&v, sha), "nono/0.80.0-0123456789ab");
    }

    #[test]
    fn steps_read_as_progress() {
        let v = Version::new(0, 80, 0);
        let step = InstallStep {
            program: Program::Nono,
            version: &v,
            arch: Arch::Aarch64,
        };
        assert_eq!(step.to_string(), "installing nono 0.80.0 (aarch64)");
    }
}
