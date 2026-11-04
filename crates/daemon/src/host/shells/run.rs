//! Running one agent command in a shell: start it (after its journal row is durable), drive it
//! to its end (timeout, cancel, waiting for input, streaming output), and build the result.
//!
//! - **Timeout** (default 600 s) and **cancel**: ETX into the PTY (SIGINT to the foreground
//!   group), then SIGKILL of the foreground group after [`KILL_AFTER`] if it is still running.
//! - **Waiting for input**: no output for [`INPUT_IDLE`] while the foreground group's leader
//!   sleeps in a terminal read flags the call (`tool.waiting_input`); unanswered for
//!   [`INPUT_TIMEOUT`] it is interrupted like a timeout.
//! - Output streams as `tool.output` (complete lines, every [`TICK`]).

use std::fs::File;
use std::sync::Arc;
use std::time::{Duration, Instant};

use controlroom_protocol::host::HostResult;
use controlroom_protocol::ids::{CallId, UnixMillis};
use controlroom_protocol::{RemotePath, RpcError};
use tokio::sync::{Notify, OwnedMutexGuard, oneshot};

use super::integration::{marker, run_line};
use super::output::Output;
use super::{Active, End, Inner, Proc, Shell, proc};
use crate::host::Host;
use crate::host::store::internal;

/// How often a running command is checked.
const TICK: Duration = Duration::from_millis(250);
/// Grace between ETX and SIGKILL of the foreground group.
const KILL_AFTER: Duration = Duration::from_secs(5);
/// Silence before a terminal read counts as waiting for input.
const INPUT_IDLE: Duration = Duration::from_secs(2);
/// How long a command may wait for input before it is interrupted.
const INPUT_TIMEOUT: Duration = Duration::from_secs(600);

impl Proc {
    /// The process whose terminal is the shell's: the shell once it reported its pid.
    fn shell_or_child(&self) -> u32 {
        self.shell_pid.unwrap_or(self.child_pid)
    }
}

/// A command that was running when the daemon restarted (from its journal row).
#[derive(Debug, Clone)]
pub struct Resume {
    /// The call.
    pub call_id: CallId,
    /// Where its output starts in the shell's transcript.
    pub start_seq: u64,
    /// When it started.
    pub started_at: UnixMillis,
    /// Its timeout.
    pub timeout_s: u32,
    /// The reset notice it carries.
    pub shell_reset: Option<String>,
}

/// A started command.
pub struct BashRun {
    shell: Arc<Shell>,
    call_id: CallId,
    done: oneshot::Receiver<End>,
    started: Instant,
    deadline: Instant,
    log_path: String,
    shell_reset: Option<String>,
    _turn: OwnedMutexGuard<()>,
}

/// Starts `command` in `shell` (reopening it if it is gone). `journal` is called with the
/// transcript position where the output will start, before the command reaches the shell;
/// if it fails, nothing runs.
pub async fn start<J, F>(
    host: &Arc<Host>,
    shell: &Arc<Shell>,
    call_id: &CallId,
    command: &str,
    timeout_s: u32,
    journal: J,
) -> Result<BashRun, RpcError>
where
    J: FnOnce(u64, Option<String>) -> F,
    F: std::future::Future<Output = Result<(), RpcError>>,
{
    let turn = shell.run_lock.clone().lock_owned().await;
    shell.ensure_running(host, None).await?;
    let workspace = host.workspaces.require(&shell.workspace_id)?;
    let control = shell
        .lock()
        .proc
        .as_ref()
        .map(|p| p.control.clone())
        .ok_or_else(|| internal("the shell exited while starting"))?;
    let start_seq = control.status().await.map_err(internal)?.last_seq + 1;
    let marker = marker(call_id.as_str());
    let command_file = workspace.files.command(&marker);
    std::fs::write(command_file.as_path(), format!("{command}\n")).map_err(internal)?;
    let log_path = workspace.files.call_log(&marker);
    let log = File::create(log_path.as_path()).ok();
    let shell_reset = shell.lock().reset_reason.clone();
    journal(start_seq, shell_reset.clone()).await?;
    let (done_tx, done) = oneshot::channel();
    {
        let mut inner = shell.lock();
        inner.reset_reason = None;
        inner.active = Some(active(call_id, marker.clone(), Output::new(log), done_tx));
    }
    shell.ready.send_replace(false);
    shell.persist(host);
    if let Err(e) = control
        .write_bytes(run_line(&marker, command_file.as_str()).as_bytes())
        .await
    {
        shell.lock().active = None;
        return Err(internal(format!("typing into the shell: {e}")));
    }
    host.shells.changed(host, shell);
    let now = Instant::now();
    Ok(BashRun {
        shell: shell.clone(),
        call_id: call_id.clone(),
        done,
        started: now,
        deadline: now + Duration::from_secs(u64::from(timeout_s)),
        log_path: log_path.to_string(),
        shell_reset,
        _turn: turn,
    })
}

fn active(call_id: &CallId, marker: String, output: Output, done: oneshot::Sender<End>) -> Active {
    Active {
        call_id: call_id.clone(),
        marker,
        started: false,
        exit: None,
        output,
        last_output: Instant::now(),
        waiting: false,
        waited: false,
        timed_out: false,
        cancelled: false,
        done: Some(done),
    }
}

/// Takes over a command that was running before a daemon restart; its shell's output is read
/// again from where the command started. Called while reattaching, with the shell locked.
pub(super) fn adopt(
    host: &Arc<Host>,
    shell: &Arc<Shell>,
    inner: &mut Inner,
    resume: &Resume,
) -> BashRun {
    let marker = marker(resume.call_id.as_str());
    let files = host.paths.workspace(&shell.workspace_id.to_string());
    let log_path = files.call_log(&marker);
    let log = File::create(log_path.as_path()).ok();
    let (done_tx, done) = oneshot::channel();
    inner.active = Some(active(&resume.call_id, marker, Output::new(log), done_tx));
    let ran_ms = UnixMillis::now().0.saturating_sub(resume.started_at.0);
    let ran = Duration::from_millis(u64::try_from(ran_ms).unwrap_or(0));
    let now = Instant::now();
    let started = now.checked_sub(ran).unwrap_or(now);
    BashRun {
        shell: shell.clone(),
        call_id: resume.call_id.clone(),
        done,
        started,
        deadline: started + Duration::from_secs(u64::from(resume.timeout_s)),
        log_path: log_path.to_string(),
        shell_reset: resume.shell_reset.clone(),
        _turn: shell
            .run_lock
            .clone()
            .try_lock_owned()
            .expect("no command runs while the daemon restores"),
    }
}

/// Drives a started command to its end; `cancel` interrupts it.
pub async fn drive(host: &Arc<Host>, mut run: BashRun, cancel: Arc<Notify>) -> HostResult {
    let shell = run.shell.clone();
    let mut ticker = tokio::time::interval(TICK);
    let mut kill_at: Option<Instant> = None;
    let mut waiting_since: Option<Instant> = None;
    let end = loop {
        tokio::select! {
            end = &mut run.done => break end.unwrap_or(End::Exited(None)),
            () = tokio::time::sleep_until(run.deadline.into()), if kill_at.is_none() => {
                shell.mark(|a| a.timed_out = true);
                kill_at = Some(interrupt(&shell).await);
            }
            () = cancel.notified(), if kill_at.is_none() => {
                shell.mark(|a| a.cancelled = true);
                kill_at = Some(interrupt(&shell).await);
            }
            _ = ticker.tick() => {
                stream_output(host, &shell, &run.call_id);
                if kill_at.is_some_and(|at| Instant::now() >= at) {
                    kill_foreground(&shell);
                    kill_at = Some(Instant::now() + KILL_AFTER);
                }
                let waiting = check_waiting(host, &shell, &run.call_id);
                match (waiting, waiting_since) {
                    (true, None) => waiting_since = Some(Instant::now()),
                    (false, _) => waiting_since = None,
                    (true, Some(since)) if since.elapsed() >= INPUT_TIMEOUT && kill_at.is_none() => {
                        shell.mark(|a| a.timed_out = true);
                        kill_at = Some(interrupt(&shell).await);
                    }
                    _ => {}
                }
            }
        }
    };
    stream_output(host, &shell, &run.call_id);
    finish(host, &run, end)
}

/// Builds the result and returns the shell to idle.
fn finish(host: &Arc<Host>, run: &BashRun, end: End) -> HostResult {
    let shell = &run.shell;
    let (active, alive) = {
        let mut inner = shell.lock();
        inner.foreground = None;
        (inner.active.take(), inner.proc.is_some())
    };
    if alive && matches!(end, End::Prompt(_)) {
        shell.ready.send_replace(true);
    }
    host.shells.changed(host, shell);
    let Some(mut active) = active else {
        return HostResult::Unknown;
    };
    if active.waiting {
        host.tools.waiting(&run.call_id, false);
    }
    let (output, truncated) = active.output.finish();
    let (exit_code, signal) = match end {
        End::Prompt(code) => (code, None),
        End::Exited(Some(exit)) => (exit.code, exit.signal.map(signal_name)),
        End::Exited(None) => (None, None),
    };
    HostResult::Bash {
        exit_code,
        signal,
        cwd: RemotePath::from(shell.cwd().as_str()),
        output,
        truncated,
        log_path: RemotePath::from(run.log_path.as_str()),
        timed_out: active.timed_out,
        cancelled: active.cancelled,
        waited_for_input: active.waited,
        shell_reset: run.shell_reset.clone(),
        duration_ms: u64::try_from(run.started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

impl Shell {
    /// Updates the running command's flags.
    fn mark(&self, f: impl FnOnce(&mut Active)) {
        if let Some(active) = self.lock().active.as_mut() {
            f(active);
        }
    }
}

/// Sends ETX; returns when to SIGKILL the foreground group.
async fn interrupt(shell: &Shell) -> Instant {
    let control = shell.lock().proc.as_ref().map(|p| p.control.clone());
    if let Some(control) = control
        && let Err(e) = control.write_bytes(b"\x03").await
    {
        tracing::warn!("interrupting shell {}: {e}", shell.id);
    }
    Instant::now() + KILL_AFTER
}

/// SIGKILLs the terminal's foreground group (if that is the shell itself, the shell dies and
/// is reopened by the next command).
fn kill_foreground(shell: &Shell) {
    let pid = shell.lock().proc.as_ref().map(Proc::shell_or_child);
    if let Some(group) = pid.and_then(proc::foreground_group) {
        tracing::info!("shell {}: killing foreground group {group}", shell.id);
        proc::kill_group(group);
    }
}

/// Streams the command's new complete lines.
fn stream_output(host: &Arc<Host>, shell: &Shell, call_id: &CallId) {
    let text = shell
        .lock()
        .active
        .as_mut()
        .map(|a| a.output.take_unsent())
        .unwrap_or_default();
    if !text.is_empty() {
        host.tools.output(call_id, text);
    }
}

/// Updates the foreground process and the waiting-for-input flag; returns whether the
/// command waits for input now.
fn check_waiting(host: &Arc<Host>, shell: &Shell, call_id: &CallId) -> bool {
    let (pid, idle) = {
        let inner = shell.lock();
        let Some(active) = inner.active.as_ref().filter(|a| a.started) else {
            return false;
        };
        (
            inner.proc.as_ref().map(Proc::shell_or_child),
            active.last_output.elapsed() >= INPUT_IDLE,
        )
    };
    let group = pid.and_then(proc::foreground_group);
    let foreground = group.and_then(proc::name);
    let waiting = idle && group.is_some_and(proc::reads_terminal);
    if idle {
        tracing::debug!(
            "shell {}: quiet; foreground group {group:?} ({foreground:?}) waiting for input: {waiting}",
            shell.id
        );
    }
    let mut changed = false;
    {
        let mut inner = shell.lock();
        if inner.foreground != foreground {
            inner.foreground = foreground;
            changed = true;
        }
        if let Some(active) = inner.active.as_mut()
            && active.waiting != waiting
        {
            active.waiting = waiting;
            active.waited |= waiting;
            changed = true;
            if waiting {
                host.tools.waiting(call_id, true);
            }
        }
    }
    if changed {
        host.shells.changed(host, shell);
    }
    waiting
}

/// A signal's name, e.g. `SIGKILL`.
pub fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        other => return format!("SIG{other}"),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_names() {
        assert_eq!(signal_name(9), "SIGKILL");
        assert_eq!(signal_name(42), "SIG42");
    }
}
