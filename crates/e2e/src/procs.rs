//! Finding and killing test processes by their command line.

use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

/// Pids of processes (other than this one) whose command line contains every one of `needles`.
pub fn find(needles: &[&str]) -> Vec<i32> {
    let me = std::process::id() as i32;
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if pid != me && needles.iter().all(|n| cmdline.contains(n)) {
            pids.push(pid);
        }
    }
    pids
}

/// SIGKILLs `pids`.
pub fn kill(pids: &[i32]) {
    for pid in pids {
        if let Some(pid) = Pid::from_raw(*pid) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

/// SIGKILLs every process whose command line mentions `needle`, until two scans in a row find
/// none (a launcher killed mid-launch may already have forked its detached child).
pub fn kill_all(needle: &str) {
    let mut quiet = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    while quiet < 2 && Instant::now() < deadline {
        let pids = find(&[needle]);
        quiet = if pids.is_empty() { quiet + 1 } else { 0 };
        kill(&pids);
        std::thread::sleep(Duration::from_millis(10));
    }
}
