//! Subscription sign-in from the UI: drives pi's own login flow (`controlroom-pi-models login`)
//! on the server, one sign-in at a time, over JSON lines:
//!
//! - the helper prints `{"step": <LoginStep>}` for every step (`choose`, `open_url`,
//!   `device_code`, `input`, `progress`, then `done` or `failed`);
//! - the agent daemon writes `{"input": "<text>"}` (an option id, an answer, or the redirect URL
//!   the user pasted; the helper hands a pasted URL to pi's prompt, or to pi's loopback callback
//!   listener on the server) and `{"cancel": true}`.
//!
//! pi stores the credential in the agent daemon's agent dir's `auth.json`, where
//! `controlroom-agentd login` puts it too. A sign-in that isn't finished within
//! [`LOGIN_TIMEOUT`] is stopped (`timed_out`); starting another one cancels it.

use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use controlroom_protocol::agentd::{LoginState, LoginStep};
use controlroom_protocol::ids::LoginId;
use controlroom_protocol::{ErrorData, Resource, RpcError};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, watch};

/// How long a sign-in may take.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// How long a cancelled helper gets to exit before it is killed.
const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// Longest helper line.
const MAX_LINE: usize = 64 * 1024;

/// What the user does to a running sign-in.
enum Control {
    Input(String),
    Cancel,
}

/// A running (or finished) sign-in.
struct Login {
    id: LoginId,
    state: watch::Sender<LoginState>,
    control: mpsc::Sender<Control>,
}

/// Called with a sign-in's final step.
pub type OnFinish = Box<dyn FnOnce(&LoginState) + Send>;

/// The sign-in slot: the current (or last) sign-in.
#[derive(Default)]
pub struct Logins {
    current: Mutex<Option<Arc<Login>>>,
}

/// One line of the helper.
#[derive(Deserialize)]
struct HelperLine {
    step: LoginStep,
}

impl Logins {
    /// Starts `command` (stdin and stdout are set up here) for `provider`, cancelling a running
    /// sign-in. Returns the first state (`progress` until the helper's first step).
    pub fn start(
        &self,
        provider: &str,
        mut command: Command,
        timeout: Duration,
        on_finish: OnFinish,
    ) -> Result<LoginState, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|e| format!("starting the models helper: {e}"))?;
        let first = LoginState {
            login_id: LoginId::generate(),
            provider: provider.to_owned(),
            seq: 1,
            step: LoginStep::Progress {
                message: "Starting".to_owned(),
            },
        };
        let (state, _) = watch::channel(first.clone());
        let (control, control_rx) = mpsc::channel(8);
        let login = Arc::new(Login {
            id: first.login_id,
            state,
            control,
        });
        let previous = self
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(login.clone());
        if let Some(previous) = previous {
            let _ = previous.control.try_send(Control::Cancel);
        }
        tokio::spawn(async move {
            drive(child, &login.state, control_rx, timeout).await;
            on_finish(&login.state.borrow());
        });
        Ok(first)
    }

    /// Answers the current step.
    pub async fn input(&self, id: LoginId, input: String) -> Result<LoginState, RpcError> {
        let login = self.get(id)?;
        if login.state.borrow().step.is_final() {
            return Err(RpcError::new(
                ErrorData::InvalidParams,
                "this sign-in is over; start again",
            ));
        }
        let _ = login.control.send(Control::Input(input)).await;
        Ok(login.state.borrow().clone())
    }

    /// The state once its `seq` is past `after`, or after `max` the current one.
    pub async fn wait(
        &self,
        id: LoginId,
        after: u32,
        max: Duration,
    ) -> Result<LoginState, RpcError> {
        let mut rx = self.get(id)?.state.subscribe();
        let _ = tokio::time::timeout(max, rx.wait_for(|s| s.seq > after)).await;
        let state = rx.borrow().clone();
        Ok(state)
    }

    /// Cancels the sign-in and returns its final state.
    pub async fn cancel(&self, id: LoginId) -> Result<LoginState, RpcError> {
        let login = self.get(id)?;
        let mut rx = login.state.subscribe();
        if !rx.borrow().step.is_final() {
            let _ = login.control.send(Control::Cancel).await;
            let _ = rx.wait_for(|s| s.step.is_final()).await;
        }
        let state = rx.borrow().clone();
        Ok(state)
    }

    fn get(&self, id: LoginId) -> Result<Arc<Login>, RpcError> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|login| login.id == id)
            .ok_or_else(|| {
                RpcError::new(
                    ErrorData::NotFound {
                        resource: Resource::SignIn,
                        id: id.to_string(),
                    },
                    "no such sign-in (another one replaced it)",
                )
            })
    }
}

/// Runs the helper until a final step, a cancel or the timeout.
async fn drive(
    mut child: Child,
    state: &watch::Sender<LoginState>,
    mut control: mpsc::Receiver<Control>,
    timeout: Duration,
) {
    let set = |step: LoginStep| {
        state.send_modify(|s| {
            s.seq += 1;
            s.step = step;
        });
    };
    let mut stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        set(failed("the models helper has no output"));
        return;
    };
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut cancelled = false;
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) if line.len() <= MAX_LINE => {
                    let Ok(HelperLine { step }) = serde_json::from_str(&line) else {
                        continue;
                    };
                    let done = step.is_final();
                    set(step);
                    if done {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => {
                    set(failed("the sign-in stopped unexpectedly"));
                    break;
                }
            },
            message = control.recv() => match message {
                Some(Control::Input(text)) => {
                    let line = serde_json::json!({ "input": text });
                    if write_line(&mut stdin, &line).await.is_err() {
                        set(failed("the sign-in stopped unexpectedly"));
                        break;
                    }
                }
                Some(Control::Cancel) | None => {
                    let _ = write_line(&mut stdin, &serde_json::json!({ "cancel": true })).await;
                    cancelled = true;
                    set(LoginStep::Cancelled);
                    break;
                }
            },
            () = &mut deadline => {
                set(LoginStep::TimedOut);
                break;
            }
        }
    }
    drop(stdin);
    let grace = if cancelled {
        CANCEL_GRACE
    } else {
        Duration::ZERO
    };
    if tokio::time::timeout(grace, child.wait()).await.is_err() || !cancelled {
        let _ = child.start_kill();
    }
    let _ = child.wait().await;
}

async fn write_line(
    stdin: &mut Option<ChildStdin>,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let stdin = stdin.as_mut().ok_or(std::io::ErrorKind::BrokenPipe)?;
    let mut line = value.to_string();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await
}

fn failed(message: &str) -> LoginStep {
    LoginStep::Failed {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted fake OAuth provider in the helper's protocol: a method choice, a sign-in page,
    /// then success for the pasted redirect URL with `code=good`, failure otherwise.
    const FAKE_PROVIDER: &str = r#"#!/bin/sh
echo '{"step": {"kind": "choose", "message": "Sign in with", "options": [{"id": "browser", "label": "Browser"}, {"id": "device", "label": "Device code"}]}}'
read -r choice
case "$choice" in
  *device*) echo '{"step": {"kind": "device_code", "user_code": "ABCD-1234", "verification_uri": "https://id.example.com/device"}}'
            read -r _; exit 0 ;;
esac
echo '{"step": {"kind": "open_url", "url": "https://id.example.com/authorize?state=s1", "instructions": null}}'
read -r pasted
case "$pasted" in
  *cancel*) exit 0 ;;
  *code=good*) echo '{"step": {"kind": "progress", "message": "Exchanging"}}'
               echo '{"step": {"kind": "done", "account": "alice@example.com"}}' ;;
  *) echo '{"step": {"kind": "failed", "message": "invalid_grant"}}'; exit 1 ;;
esac
"#;

    fn command(dir: &std::path::Path, script: &str) -> Command {
        let path = dir.join("fake-helper");
        crate::models::test_support::write_executable(&path, script);
        Command::new(path)
    }

    async fn next(logins: &Logins, state: &LoginState) -> LoginState {
        logins
            .wait(state.login_id, state.seq, Duration::from_secs(10))
            .await
            .unwrap()
    }

    /// Waits until the step is `matches`.
    async fn until(
        logins: &Logins,
        mut state: LoginState,
        matches: fn(&LoginStep) -> bool,
    ) -> LoginState {
        while !matches(&state.step) {
            let after = next(logins, &state).await;
            assert!(after.seq > state.seq, "no progress from {state:?}");
            state = after;
        }
        state
    }

    #[tokio::test]
    async fn url_then_paste_signs_in() {
        let t = tempfile::tempdir().unwrap();
        let logins = Logins::default();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let state = logins
            .start(
                "sub",
                command(t.path(), FAKE_PROVIDER),
                LOGIN_TIMEOUT,
                Box::new(move |s| {
                    let _ = done_tx.send(s.step.clone());
                }),
            )
            .unwrap();
        let state = until(&logins, state, |s| matches!(s, LoginStep::Choose { .. })).await;
        logins
            .input(state.login_id, "browser".into())
            .await
            .unwrap();
        let state = until(&logins, state, |s| matches!(s, LoginStep::OpenUrl { .. })).await;
        logins
            .input(
                state.login_id,
                "http://localhost:1455/callback?code=good&state=s1".into(),
            )
            .await
            .unwrap();
        let state = until(&logins, state, LoginStep::is_final).await;
        assert_eq!(
            state.step,
            LoginStep::Done {
                account: Some("alice@example.com".into())
            }
        );
        assert!(matches!(done_rx.await.unwrap(), LoginStep::Done { .. }));
        // Over: no more input.
        assert!(logins.input(state.login_id, "x".into()).await.is_err());
    }

    #[tokio::test]
    async fn failure_cancel_timeout_and_replacement() {
        let t = tempfile::tempdir().unwrap();
        let logins = Logins::default();
        let start = |timeout| {
            logins
                .start(
                    "sub",
                    command(t.path(), FAKE_PROVIDER),
                    timeout,
                    Box::new(|_| {}),
                )
                .unwrap()
        };

        // A bad redirect URL: the provider's error.
        let state = start(LOGIN_TIMEOUT);
        let state = until(&logins, state, |s| matches!(s, LoginStep::Choose { .. })).await;
        logins
            .input(state.login_id, "browser".into())
            .await
            .unwrap();
        let state = until(&logins, state, |s| matches!(s, LoginStep::OpenUrl { .. })).await;
        logins
            .input(state.login_id, "code=bad".into())
            .await
            .unwrap();
        let state = until(&logins, state, LoginStep::is_final).await;
        assert_eq!(state.step, failed("invalid_grant"));

        // Device code, then cancelled.
        let state = start(LOGIN_TIMEOUT);
        let state = until(&logins, state, |s| matches!(s, LoginStep::Choose { .. })).await;
        logins.input(state.login_id, "device".into()).await.unwrap();
        let state = until(&logins, state, |s| {
            matches!(s, LoginStep::DeviceCode { .. })
        })
        .await;
        let cancelled = logins.cancel(state.login_id).await.unwrap();
        assert_eq!(cancelled.step, LoginStep::Cancelled);

        // Nobody answers.
        let state = start(Duration::from_millis(200));
        let state = until(&logins, state, LoginStep::is_final).await;
        assert_eq!(state.step, LoginStep::TimedOut);

        // A new sign-in replaces (and cancels) the running one.
        let first = start(LOGIN_TIMEOUT);
        let first = until(&logins, first, |s| matches!(s, LoginStep::Choose { .. })).await;
        let second = start(LOGIN_TIMEOUT);
        assert!(
            logins
                .wait(first.login_id, 0, Duration::ZERO)
                .await
                .is_err()
        );
        assert_ne!(first.login_id, second.login_id);

        // A helper that dies.
        let dying = logins
            .start(
                "sub",
                command(t.path(), "#!/bin/sh\nexit 3\n"),
                LOGIN_TIMEOUT,
                Box::new(|_| {}),
            )
            .unwrap();
        let state = until(&logins, dying, LoginStep::is_final).await;
        assert!(matches!(state.step, LoginStep::Failed { .. }));
    }
}
