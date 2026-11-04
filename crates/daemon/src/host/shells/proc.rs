//! What a terminal's foreground process is doing, from `/proc` (no access to the PTY needed):
//! the foreground process group of the terminal a process is attached to (`tpgid`, field 8
//! of `/proc/<pid>/stat`), its name, and whether it sleeps in a terminal read.

use rustix::process::{Pid, Signal, kill_process_group};

/// Fields after the `(comm)` of `/proc/<pid>/stat`.
fn stat_fields(pid: i32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

/// The foreground process group of the terminal `pid` is attached to.
pub fn foreground_group(pid: u32) -> Option<i32> {
    let pid = i32::try_from(pid).ok()?;
    // After the state: ppid, pgrp, session, tty_nr, tpgid.
    let tpgid: i32 = stat_fields(pid)?.get(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid)
}

/// The name of process `pid`.
pub fn name(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim().to_owned())
}

/// Whether process `pid` sleeps reading a terminal (`n_tty_read`, which shows as `wait_woken`
/// on current kernels).
pub fn reads_terminal(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/wchan"))
        .is_ok_and(|w| matches!(w.trim(), "n_tty_read" | "wait_woken"))
}

/// SIGKILLs process group `pgid` (never 0 or 1).
pub fn kill_group(pgid: i32) {
    if let Some(pid) = Some(pgid).filter(|&p| p > 1).and_then(Pid::from_raw) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let me = std::process::id();
        assert!(name(i32::try_from(me).unwrap()).is_some());
        // The test runner may or may not have a terminal; either way this must not panic.
        let _ = foreground_group(me);
        assert!(!reads_terminal(i32::try_from(me).unwrap()));
    }
}
