//! Removing old daemon versions, and the nono versions only they used.

use super::platform::Version;
use super::{BootstrapError, RemoteShell, fields, run_script};
use crate::shell::quote;

/// What [`prune_versions`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The removed daemon versions.
    pub versions: Vec<String>,
    /// The removed nono builds (directory names).
    pub nono: Vec<String>,
}

/// Removes every `versions/<ver>` directory except `current`'s target, the
/// versions in `keep`, and any version a process of this user still runs
/// from (by `/proc/<pid>/exe` or `argv[0]`): detached supervisors keep
/// running from the version that started them.
///
/// Then removes every `nono/<build>` directory (`<version>-<sha>`, or `<version>` as installed
/// before nono was keyed by content) that no remaining daemon version links to and no process
/// of this user runs from (a sandbox started by a since-pruned daemon).
pub async fn prune_versions<S: RemoteShell>(
    shell: &S,
    keep: &[Version],
) -> Result<Pruned, BootstrapError> {
    let keep: Vec<String> = keep.iter().map(Version::to_string).collect();
    let script = format!(
        r#"keep=" "{keep}" "
cur=$(readlink "$base/current" 2>/dev/null || true)
inuse=" "
nono_inuse=" "
for p in /proc/[0-9]*; do
  [ -O "$p" ] || continue
  exe=$(readlink "$p/exe" 2>/dev/null || true)
  arg0=$(tr '\000' '\n' 2>/dev/null < "$p/cmdline" | head -n 1)
  for path in "$exe" "$arg0"; do
    case "$path" in
      "$base/versions/"*/*) v=${{path#"$base/versions/"}}; inuse="$inuse${{v%%/*}} " ;;
      "$base/nono/"*/*) v=${{path#"$base/nono/"}}; nono_inuse="$nono_inuse${{v%%/*}} " ;;
    esac
  done
done
for d in "$base/versions/"*/; do
  [ -d "$d" ] || continue
  v=$(basename "$d")
  [ "versions/$v" = "$cur" ] && continue
  case "$keep$inuse" in *" $v "*) continue ;; esac
  rm -rf "$d"
  printf 'removed=%s\n' "$v"
done
linked=" "
for l in "$base/versions/"*/nono; do
  t=$(readlink "$l" 2>/dev/null) || continue
  case "$t" in
    ../../nono/*/nono) v=${{t#../../nono/}}; linked="$linked${{v%/nono}} " ;;
  esac
done
for d in "$base/nono/"*/; do
  [ -d "$d" ] || continue
  v=$(basename "$d")
  case "$linked$nono_inuse" in *" $v "*) continue ;; esac
  rm -rf "$d"
  printf 'removed_nono=%s\n' "$v"
done"#,
        keep = quote(&keep.join(" ")),
    );
    let out = run_script(shell, "prune versions", &script, b"").await?;
    let stdout = out.stdout_str();
    Ok(Pruned {
        versions: fields(&stdout, "removed").map(str::to_owned).collect(),
        nono: fields(&stdout, "removed_nono").map(str::to_owned).collect(),
    })
}
