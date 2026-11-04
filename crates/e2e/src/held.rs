//! Remote commands held at a gate: a command that records its start, blocks reading a FIFO
//! until the test releases it, and records its end, so a test can act while it is running
//! (cut a link, kill a daemon) without sleeping.

use std::path::{Path, PathBuf};

use controlroom_controller::test_support::wait_until;

/// One held command's files, in a directory on the host.
pub struct Held {
    /// Gets a line per start.
    ran: PathBuf,
    /// The FIFO the command reads.
    gate: PathBuf,
    /// Gets a line when the command passed the gate.
    done: PathBuf,
}

impl Held {
    /// Prepares `<dir>/<name>.{ran,gate,done}` (the gate is a FIFO).
    pub fn new(dir: &Path, name: &str) -> Self {
        let held = Self {
            ran: dir.join(format!("{name}.ran")),
            gate: dir.join(format!("{name}.gate")),
            done: dir.join(format!("{name}.done")),
        };
        let status = std::process::Command::new("mkfifo")
            .arg(&held.gate)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo {}", held.gate.display());
        held
    }

    /// A shell command: `before`, then the gate, then `after`.
    pub fn command(&self, before: &str, after: &str) -> String {
        format!(
            "echo x >> {ran}; {before}; cat {gate} > /dev/null; {after}; echo x >> {done}",
            ran = self.ran.display(),
            gate = self.gate.display(),
            done = self.done.display(),
        )
    }

    /// Waits until the command started.
    pub async fn started(&self) {
        let ran = self.ran.clone();
        wait_until("the held command starts", || ran.exists()).await;
    }

    /// Opens the gate (the command is reading it) and waits until the command passed it.
    pub async fn release(&self) {
        let gate = self.gate.clone();
        tokio::task::spawn_blocking(move || std::fs::write(gate, "go\n").unwrap())
            .await
            .unwrap();
        let done = self.done.clone();
        wait_until("the held command passes its gate", || done.exists()).await;
    }

    /// How many times the command started.
    pub fn runs(&self) -> usize {
        std::fs::read_to_string(&self.ran)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
}
