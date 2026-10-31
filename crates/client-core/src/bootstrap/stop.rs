//! Stopping the running `controlroomd serve`, so the next
//! `controlroomd connect` starts the installed version.
//!
//! Only the remote user's own processes whose command line is
//! `~/.local/share/controlroom/<...>/controlroomd serve [...]` are signaled
//! (SIGTERM, which the daemon handles gracefully). Supervisors
//! (`controlroomd supervise`) are left alone: agents keep running and the new
//! daemon reattaches to them.

use std::time::Duration;

use super::{BootstrapError, RemoteShell, field, run_script};

fn stop_script(wait: Duration) -> String {
    let tenths = wait.as_millis().div_ceil(100);
    format!(
        r#"limit={tenths}
n=0
pids=
for p in /proc/[0-9]*; do
  [ -O "$p" ] || continue
  cmd=$(tr '\000' ' ' 2>/dev/null < "$p/cmdline") || continue
  case "$cmd" in
    "$base/"*"/controlroomd serve"|"$base/"*"/controlroomd serve "*)
      pid=${{p#/proc/}}
      if kill -TERM "$pid" 2>/dev/null; then n=$((n+1)); pids="$pids $pid"; fi ;;
  esac
done
alive() {{
  state=$(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -d ' ' -f 1)
  [ -n "$state" ] && [ "$state" != Z ]
}}
i=0
for pid in $pids; do
  while alive "$pid" && [ "$i" -lt "$limit" ]; do sleep 0.1; i=$((i+1)); done
done
remaining=
for pid in $pids; do
  if alive "$pid"; then remaining="$remaining $pid"; fi
done
printf 'stopped=%s\nremaining=%s\n' "$n" "$remaining""#
    )
}

/// Stops the running daemon(s), waiting up to `wait` for them to exit.
///
/// Returns how many were signaled. Fails with
/// [`BootstrapError::DaemonStillRunning`] if any is still alive after `wait`.
pub async fn stop_daemon<S: RemoteShell>(shell: &S, wait: Duration) -> Result<u32, BootstrapError> {
    let out = run_script(shell, "stop daemon", &stop_script(wait), b"").await?;
    let text = out.stdout_str();
    let parse_error = || BootstrapError::Parse(format!("stop daemon: {text:?}"));
    let stopped = field(&text, "stopped")
        .and_then(|n| n.parse().ok())
        .ok_or_else(parse_error)?;
    let pids = field(&text, "remaining")
        .ok_or_else(parse_error)?
        .split_whitespace()
        .map(|pid| pid.parse().map_err(|_| parse_error()))
        .collect::<Result<Vec<u32>, _>>()?;
    if pids.is_empty() {
        Ok(stopped)
    } else {
        Err(BootstrapError::DaemonStillRunning { pids })
    }
}
