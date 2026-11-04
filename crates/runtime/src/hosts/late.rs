//! Late results: a tool call that failed with `connection_lost` may still run on its host.
//! Once the host is back, `tool.attach` fetches what became of it (the result, `lost`, or
//! `not_found` when it never started), and the outcome is injected into the session (origin
//! `tool_late`, waking the agent). Late calls are persisted, so an agent daemon restart fetches
//! them again; an injected outcome is removed from the list in the same transaction.

use std::sync::Arc;

use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{HostRequest, HostResult, ToolCallRef};
use controlroom_protocol::ids::{HostOpId, SessionId};
use controlroom_protocol::timeline::InjectOrigin;
use controlroom_protocol::{ErrorData, RpcError};

use super::calls::parse;
use super::records::LateCall;
use crate::core::Core;
use crate::model::Injection;
use crate::runtime::Inner;

/// Bash output kept in an injected late result (its tail).
const OUTPUT_CHARS: usize = 8000;

/// Fetches a late call's outcome (waiting for its host) and injects it.
pub(crate) async fn attach(inner: Arc<Inner>, id: SessionId, late: LateCall) {
    let response = inner.bridge.request(HostOpRequest {
        id: HostOpId::new(format!("attach:{}", late.call_id)),
        machine_id: late.machine_id,
        session_id: Some(id),
        request: HostRequest::ToolAttach(ToolCallRef {
            call_id: late.call_id.clone(),
        }),
    });
    let Ok(outcome) = response.await else {
        return;
    };
    if let Err(e) = inner.transact(|core| core.late_outcome(id, &late, outcome)) {
        tracing::warn!(
            "session {id}: injecting the late result of {}: {e}",
            late.call_id
        );
    }
}

impl Core {
    /// Injects a late call's outcome and forgets the call.
    fn late_outcome(
        &mut self,
        id: SessionId,
        late: &LateCall,
        outcome: Result<serde_json::Value, RpcError>,
    ) -> Result<(), RpcError> {
        let session = self.session_mut(id)?;
        let Some(index) = session
            .data
            .late
            .iter()
            .position(|l| l.call_id == late.call_id)
        else {
            return Ok(());
        };
        session.data.late.remove(index);
        if outcome.is_ok() {
            self.acks
                .entry(late.machine_id)
                .or_default()
                .push(late.call_id.clone());
        }
        if self.session(id)?.data.closed() {
            return Ok(());
        }
        let machine = self.machines.name(&late.machine_id);
        let text = late_text(&machine, late, outcome);
        let label = format!("late result: {}", late.what);
        let now = self.clock.now();
        self.inject(
            id,
            Injection::new(InjectOrigin::ToolLate, label, text, true, now),
        )
    }
}

/// The injected text: what became of the call.
fn late_text(
    machine: &str,
    late: &LateCall,
    outcome: Result<serde_json::Value, RpcError>,
) -> String {
    let head = format!(
        "[controlroom] {machine} is reachable again. Your earlier call {} ({}) failed because \
         {machine} was unreachable;",
        late.call_id, late.what
    );
    match outcome.and_then(parse::<HostResult>) {
        Ok(HostResult::Bash {
            exit_code,
            signal,
            output,
            log_path,
            ..
        }) => {
            let status = match (exit_code, signal) {
                (Some(code), _) => format!("exit {code}"),
                (None, Some(signal)) => format!("killed by {signal}"),
                (None, None) => "no exit status".to_owned(),
            };
            let skip = output.chars().count().saturating_sub(OUTPUT_CHARS);
            let tail: String = output.chars().skip(skip).collect();
            let cut = if skip > 0 {
                format!(
                    " (the output's last {OUTPUT_CHARS} characters; all of it is in {machine}:{log_path})"
                )
            } else {
                String::new()
            };
            format!("{head} it ran once on {machine} and ended with {status}{cut}:\n{tail}")
        }
        Ok(_) => format!("{head} it completed on {machine}; repeat it if you need its result."),
        Err(e) => match e.data {
            Some(ErrorData::NotFound { .. }) => {
                format!("{head} it never reached {machine}, so it didn't run.")
            }
            Some(ErrorData::Lost { may_have_run }) => {
                let ran = if may_have_run {
                    "it may have partly run"
                } else {
                    "it had no effect"
                };
                format!("{head} {machine}'s daemon restarted while it ran: {ran}.")
            }
            _ => format!("{head} its outcome is unknown: {}.", e.message),
        },
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::ids::{CallId, MachineId, RemotePath, UnixMillis};

    use super::*;

    fn late() -> LateCall {
        LateCall {
            call_id: CallId::new("call-1"),
            machine_id: MachineId::generate(),
            what: "bash `make`".into(),
            failed_at: UnixMillis(0),
        }
    }

    #[test]
    fn late_texts_say_what_became_of_the_call() {
        let result = HostResult::Bash {
            exit_code: Some(0),
            signal: None,
            cwd: RemotePath::new("/home/alice"),
            output: "built\n".into(),
            truncated: false,
            log_path: RemotePath::new("/tmp/log"),
            timed_out: false,
            cancelled: false,
            waited_for_input: false,
            shell_reset: None,
            duration_ms: 5,
        };
        let text = late_text("devbox", &late(), Ok(serde_json::to_value(result).unwrap()));
        assert!(
            text.contains("it ran once on devbox and ended with exit 0:\nbuilt"),
            "{text}"
        );
        let never = RpcError::new(
            ErrorData::NotFound {
                resource: controlroom_protocol::Resource::Call,
                id: "call-1".into(),
            },
            "no call",
        );
        assert!(late_text("devbox", &late(), Err(never)).contains("didn't run"));
        let lost = RpcError::new(ErrorData::Lost { may_have_run: true }, "lost");
        assert!(late_text("devbox", &late(), Err(lost)).contains("may have partly run"));
    }
}
