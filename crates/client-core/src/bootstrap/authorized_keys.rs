//! Editing `~/.ssh/authorized_keys` for key rotation.
//!
//! The editing itself is pure ([`append_key`], [`remove_key`]); the remote
//! helpers read the file, edit it locally, and write it back through a temp
//! file plus `mv`, preserving the existing file's mode (`cp -p`).

use super::{BootstrapError, RemoteShell, run_script};
use crate::shell::quote;

/// Splits an `authorized_keys` line into `(key type, base64 blob)`, skipping
/// any leading options (which may contain quoted spaces). Returns `None` for
/// blank lines, comments and lines without a recognizable key.
pub fn key_identity(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let tokens = tokenize(line);
    tokens.windows(2).find_map(|w| {
        let (ty, blob) = (w[0], w[1]);
        (is_key_type(ty) && is_base64(blob)).then_some((ty, blob))
    })
}

fn is_key_type(t: &str) -> bool {
    t.starts_with("ssh-") || t.starts_with("ecdsa-sha2-") || t.starts_with("sk-")
}

fn is_base64(t: &str) -> bool {
    t.len() >= 16
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

/// Whitespace-separated words, treating `"..."` (with `\"` escapes) as part of a word.
fn tokenize(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => escaped = true,
            '"' => {
                in_quotes = !in_quotes;
                start.get_or_insert(i);
            }
            c if c.is_whitespace() && !in_quotes => {
                if let Some(s) = start.take() {
                    out.push(&line[s..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    out
}

/// Appends `key_line` unless the same key (type and blob, ignoring options and
/// comment) is already present. Returns the new contents, or `None` if unchanged.
pub fn append_key(contents: &str, key_line: &str) -> Option<String> {
    let wanted = key_identity(key_line)?;
    if contents.lines().any(|l| key_identity(l) == Some(wanted)) {
        return None;
    }
    let mut out = contents.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(key_line.trim());
    out.push('\n');
    Some(out)
}

/// Removes every line carrying the same key as `key_line`, leaving all other
/// lines byte-for-byte intact. Returns the new contents and how many lines
/// were removed.
pub fn remove_key(contents: &str, key_line: &str) -> (String, usize) {
    let Some(wanted) = key_identity(key_line) else {
        return (contents.to_owned(), 0);
    };
    let mut out = String::with_capacity(contents.len());
    let mut removed = 0;
    for line in contents.split_inclusive('\n') {
        if key_identity(line) == Some(wanted) {
            removed += 1;
        } else {
            out.push_str(line);
        }
    }
    (out, removed)
}

/// Prints `sum=<cksum>` (or `sum=absent`), then the file's contents.
const READ_SCRIPT: &str = r#"f="$HOME/.ssh/authorized_keys"
if [ -e "$f" ]; then
  printf 'sum=%s\n' "$(cksum < "$f")"
  cat "$f"
else
  echo 'sum=absent'
fi"#;

/// Writes stdin to authorized_keys if its checksum still matches what was
/// read (a guard against a concurrent edit). Refuses to replace a symlink,
/// which usually means the file is managed elsewhere (e.g. by a
/// configuration manager). The existing file's mode is kept (`cp -p`).
fn write_script(expected_sum: &str) -> String {
    format!(
        r#"set -eu
umask 077
d="$HOME/.ssh"
f="$d/authorized_keys"
mkdir -p "$d"
if [ -L "$f" ]; then echo "$f is a symlink (managed elsewhere?); refusing to replace it" >&2; exit 5; fi
if [ -e "$f" ]; then cur=$(cksum < "$f"); else cur=absent; fi
if [ "$cur" != {expected} ]; then echo "$f changed while editing it; retry" >&2; exit 6; fi
tmp="$f.controlroom-$$"
trap 'rm -f "$tmp"' EXIT
if [ -e "$f" ]; then cp -p "$f" "$tmp"; fi
cat > "$tmp"
mv -f "$tmp" "$f"
trap - EXIT"#,
        expected = quote(expected_sum)
    )
}

async fn read_remote<S: RemoteShell>(shell: &S) -> Result<(String, String), BootstrapError> {
    let out = run_script(shell, "read authorized_keys", READ_SCRIPT, b"").await?;
    let text = String::from_utf8(out.stdout)
        .map_err(|_| BootstrapError::Parse("authorized_keys is not UTF-8".into()))?;
    let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
    let sum = first
        .strip_prefix("sum=")
        .ok_or_else(|| BootstrapError::Parse(format!("unexpected output {first:?}")))?;
    Ok((rest.to_owned(), sum.to_owned()))
}

async fn write_remote<S: RemoteShell>(
    shell: &S,
    contents: &str,
    expected_sum: &str,
) -> Result<(), BootstrapError> {
    run_script(
        shell,
        "write authorized_keys",
        &write_script(expected_sum),
        contents.as_bytes(),
    )
    .await?;
    Ok(())
}

/// Adds `key_line` to the remote `~/.ssh/authorized_keys` if absent.
/// Returns whether the file changed.
pub async fn add_authorized_key<S: RemoteShell>(
    shell: &S,
    key_line: &str,
) -> Result<bool, BootstrapError> {
    if key_identity(key_line).is_none() {
        return Err(BootstrapError::Parse(format!(
            "not a public key line: {key_line:?}"
        )));
    }
    let (contents, sum) = read_remote(shell).await?;
    match append_key(&contents, key_line) {
        None => Ok(false),
        Some(new) => {
            write_remote(shell, &new, &sum).await?;
            Ok(true)
        }
    }
}

/// Removes `key_line`'s key from the remote `~/.ssh/authorized_keys`.
/// Returns whether the file changed.
pub async fn remove_authorized_key<S: RemoteShell>(
    shell: &S,
    key_line: &str,
) -> Result<bool, BootstrapError> {
    let (contents, sum) = read_remote(shell).await?;
    let (new, removed) = remove_key(&contents, key_line);
    if removed == 0 {
        return Ok(false);
    }
    write_remote(shell, &new, &sum).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEvYYFqtmyAwG91pMgBKerb/Ly0i8a4OUZAXeWRT7bdw controlroom@devbox";
    const B: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBGU2zONOpl+OSphfvk6EeQqbUyRvOTspW+oO1Kz2SwXbkkMB30FMAWUNAUlyO8PEmFZcDXShIqcaQSGPSh9sjb4= alice@laptop";

    #[test]
    fn identity_skips_options_and_comments() {
        let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEvYYFqtmyAwG91pMgBKerb/Ly0i8a4OUZAXeWRT7bdw";
        assert_eq!(key_identity(A), Some(("ssh-ed25519", blob)));
        let with_opts = format!(
            r#"from="10.0.0.0/8",command="echo \"hi there\"",no-pty ssh-ed25519 {blob} other comment"#
        );
        assert_eq!(key_identity(&with_opts), Some(("ssh-ed25519", blob)));
        assert_eq!(key_identity("# ssh-ed25519 AAAA"), None);
        assert_eq!(key_identity("   "), None);
        assert_eq!(key_identity("garbage line"), None);
    }

    #[test]
    fn append_is_idempotent() {
        let start = format!("# managed by hand\n{B}");
        let once = append_key(&start, A).unwrap();
        assert_eq!(once, format!("# managed by hand\n{B}\n{A}\n"));
        assert_eq!(append_key(&once, A), None);
        // Same key with a different comment or options counts as present.
        let renamed = A.replace("controlroom@devbox", "renamed");
        assert_eq!(append_key(&once, &renamed), None);
        assert_eq!(append_key("", A).unwrap(), format!("{A}\n"));
        assert_eq!(append_key("x", "not a key"), None);
    }

    #[test]
    fn remove_only_matching_lines() {
        let contents = format!("{B}\n\n# comment\nrestrict {A}\n{A}\ntrailing-no-newline");
        let (out, n) = remove_key(&contents, A);
        assert_eq!(n, 2);
        assert_eq!(out, format!("{B}\n\n# comment\ntrailing-no-newline"));
        let (same, n) = remove_key(&out, A);
        assert_eq!((same.as_str(), n), (out.as_str(), 0));
        let (last, n) = remove_key(&format!("{B}\n{A}"), A);
        assert_eq!((last, n), (format!("{B}\n"), 1));
    }
}
