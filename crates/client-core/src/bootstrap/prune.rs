//! Removing old daemon versions.

use super::platform::Version;
use super::{BootstrapError, RemoteShell, fields, run_script};
use crate::shell::quote;

/// Removes every `versions/<ver>` directory except `current`'s target, the
/// versions in `keep`, and any version a process of this user still runs
/// from (by `/proc/<pid>/exe` or `argv[0]`): detached supervisors keep
/// running from the version that started them.
///
/// Returns the removed versions.
pub async fn prune_versions<S: RemoteShell>(
    shell: &S,
    keep: &[Version],
) -> Result<Vec<String>, BootstrapError> {
    let keep: Vec<String> = keep.iter().map(Version::to_string).collect();
    let script = format!(
        r#"keep=" "{keep}" "
cur=$(readlink "$base/current" 2>/dev/null || true)
inuse=" "
for p in /proc/[0-9]*; do
  [ -O "$p" ] || continue
  exe=$(readlink "$p/exe" 2>/dev/null || true)
  arg0=$(tr '\000' '\n' 2>/dev/null < "$p/cmdline" | head -n 1)
  for path in "$exe" "$arg0"; do
    case "$path" in
      "$base/versions/"*/*) v=${{path#"$base/versions/"}}; inuse="$inuse${{v%%/*}} " ;;
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
done"#,
        keep = quote(&keep.join(" ")),
    );
    let out = run_script(shell, "prune versions", &script, b"").await?;
    let stdout = out.stdout_str();
    Ok(fields(&stdout, "removed").map(str::to_owned).collect())
}
