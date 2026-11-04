//! An in-process fake agent daemon on a unix socket: it answers the controller's `hello`,
//! serves a sessions feed the test edits, records every host bridge call (`hostops.respond`,
//! `hostops.notify`, `hostops.machines`) and every pass-through call, and lets the test publish
//! host requests (`hostops.request`) and withdraw them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use controlroom_protocol::agentd::notifications::{
    HostOpsRequest, HostOpsWithdraw, SessionsEvents,
};
use controlroom_protocol::agentd::{
    HostMachine, HostOpRequest, HostOpsNotifyParams, HostOpsRequestParams, HostOpsRespondParams,
    HostOpsSubscribeResult, HostOpsWithdrawParams, SessionCreateParams, SessionCreateResult,
    SessionSummary, SessionsChange, SessionsEvent, SessionsEventsParams, SessionsSnapshot,
    SessionsSubscribeParams, SessionsSubscribeResult,
};
use controlroom_protocol::daemon::{DaemonSettings, HelloResult, PromptResult, RetentionPolicy};
use controlroom_protocol::ids::{DaemonId, HostOpId, SessionId, SubscriptionId, WorkspaceId};
use controlroom_protocol::session::{Availability, SessionState, Workspace};
use controlroom_protocol::ui::Connectivity;
use controlroom_protocol::{
    AGENTD_PROTOCOL_VERSION, ErrorData, Rev, RpcError, RpcOutcome, UnixMillis,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;

use super::wait_until;
use super::wire::{Peer, serve};

#[derive(Default)]
struct State {
    peer: Option<Peer>,
    hellos: u32,
    rev: u64,
    sessions: Vec<SessionSummary>,
    sessions_sub: Option<SubscriptionId>,
    hostops_sub: Option<SubscriptionId>,
    pending: Vec<HostOpRequest>,
    responses: Vec<HostOpsRespondParams>,
    notifies: Vec<HostOpsNotifyParams>,
    machines: Vec<Vec<HostMachine>>,
    calls: Vec<(String, Value)>,
    created: HashMap<String, SessionCreateResult>,
    /// The latest `session.subscribe` per session.
    session_subs: HashMap<SessionId, SubscriptionId>,
    /// `session.prompt` results by op id (a repeat returns the first), in order.
    prompts: Vec<(String, Value, Value)>,
    /// Refusing connections.
    down: bool,
}

/// The fake agent daemon. Dropping it stops listening and closes its connection.
pub struct FakeAgentd {
    path: PathBuf,
    state: Arc<Mutex<State>>,
    accept: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl Drop for FakeAgentd {
    fn drop(&mut self) {
        self.accept.abort();
        if let Some(peer) = self.state.lock().unwrap().peer.take() {
            peer.close();
        }
    }
}

impl FakeAgentd {
    /// Listens on a fresh socket.
    pub async fn start() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("cragentd")
            .tempdir()
            .unwrap();
        let path = dir.path().join("agentd.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let state: Arc<Mutex<State>> = Arc::default();
        let accept = tokio::spawn({
            let state = state.clone();
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    if state.lock().unwrap().down {
                        continue;
                    }
                    let st = state.clone();
                    let peer = serve(stream, move |peer, method, params| {
                        let st = st.clone();
                        async move { handle(&st, peer, &method, params) }
                    });
                    // One controller at a time: a new connection replaces the old one.
                    if let Some(old) = state.lock().unwrap().peer.replace(peer) {
                        old.close();
                    }
                }
            }
        });
        Self {
            path,
            state,
            accept,
            _dir: dir,
        }
    }

    /// The socket path, for `ControllerConfig::agentd_socket`.
    pub fn socket(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// How many `hello`s it answered.
    pub fn hellos(&self) -> u32 {
        self.lock().hellos
    }

    /// Waits for the controller's next `hostops.subscribe` after `hello` number `n`.
    pub async fn wait_connected(&self, n: u32) {
        wait_until("the controller subscribes to host requests", || {
            let st = self.lock();
            st.hellos >= n && st.hostops_sub.is_some()
        })
        .await;
    }

    /// Drops the controller's connection (it reconnects).
    pub fn drop_connection(&self) {
        let mut st = self.lock();
        st.hostops_sub = None;
        st.sessions_sub = None;
        if let Some(peer) = st.peer.take() {
            peer.close();
        }
    }

    /// Publishes a host request (pending until answered or withdrawn).
    pub fn request(&self, request: HostOpRequest) {
        let mut st = self.lock();
        st.pending.push(request.clone());
        if let (Some(peer), Some(sub)) = (&st.peer, st.hostops_sub) {
            peer.notify::<HostOpsRequest>(HostOpsRequestParams {
                subscription_id: sub,
                requests: vec![request],
            });
        }
    }

    /// Withdraws a pending host request.
    pub fn withdraw(&self, id: &HostOpId) {
        let mut st = self.lock();
        st.pending.retain(|r| &r.id != id);
        if let (Some(peer), Some(sub)) = (&st.peer, st.hostops_sub) {
            peer.notify::<HostOpsWithdraw>(HostOpsWithdrawParams {
                subscription_id: sub,
                ids: vec![id.clone()],
            });
        }
    }

    /// Every answer received for `id`, in order.
    pub fn responses(&self, id: &HostOpId) -> Vec<RpcOutcome<Value>> {
        self.lock()
            .responses
            .iter()
            .filter(|r| &r.id == id)
            .map(|r| r.outcome.clone())
            .collect()
    }

    /// Waits for the first answer to `id`.
    pub async fn response(&self, id: &HostOpId) -> RpcOutcome<Value> {
        wait_until(&format!("an answer to {id}"), || {
            !self.responses(id).is_empty()
        })
        .await;
        self.responses(id).remove(0)
    }

    /// Every forwarded host notification, in order.
    pub fn notifies(&self) -> Vec<HostOpsNotifyParams> {
        self.lock().notifies.clone()
    }

    /// The latest machine report.
    pub fn machines(&self) -> Option<Vec<HostMachine>> {
        self.lock().machines.last().cloned()
    }

    /// Waits until the latest machine report satisfies `pred`; returns it.
    pub async fn wait_machines(
        &self,
        what: &str,
        pred: impl Fn(&[HostMachine]) -> bool,
    ) -> Vec<HostMachine> {
        wait_until(what, || self.machines().is_some_and(|m| pred(&m))).await;
        self.machines().unwrap()
    }

    /// Every call of `method` received, with its params.
    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.lock()
            .calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// Every prompt delivered (once per op id), in order.
    pub fn prompts(&self) -> Vec<Value> {
        self.lock()
            .prompts
            .iter()
            .map(|(_, p, _)| p.clone())
            .collect()
    }

    /// Takes the agent daemon away (the controller's connection drops and new ones are
    /// refused) or brings it back.
    pub fn set_down(&self, down: bool) {
        self.lock().down = down;
        if down {
            self.drop_connection();
        }
    }

    /// Adds or replaces a session and publishes it.
    pub fn upsert_session(&self, session: SessionSummary) {
        let mut st = self.lock();
        st.sessions.retain(|s| s.id != session.id);
        st.sessions.push(session.clone());
        publish(
            &mut st,
            SessionsEvent::SessionUpserted {
                session: Box::new(session),
            },
        );
    }

    /// Removes a session and publishes it.
    pub fn remove_session(&self, id: SessionId) {
        let mut st = self.lock();
        st.sessions.retain(|s| s.id != id);
        publish(&mut st, SessionsEvent::SessionRemoved { id });
    }

    /// Streams `text` into session `id`'s timeline at `seq` (to its latest subscription).
    pub fn append_text(&self, id: SessionId, seq: u64, text: &str) {
        let st = self.lock();
        let (Some(peer), Some(sub)) = (&st.peer, st.session_subs.get(&id)) else {
            panic!("session {id} has no subscription");
        };
        peer.notify::<controlroom_protocol::agentd::notifications::SessionEvents>(
            controlroom_protocol::daemon::SessionEventsParams {
                subscription_id: *sub,
                id,
                head_seq: controlroom_protocol::Seq(seq),
                ops: vec![controlroom_protocol::timeline::TimelineOp::AppendText {
                    id: controlroom_protocol::ids::ItemId::new("m1"),
                    text: text.to_owned(),
                    seq_from: controlroom_protocol::Seq(seq),
                    seq_to: controlroom_protocol::Seq(seq),
                }],
                raw: vec![],
                detail: None,
            },
        );
    }

    /// The current summary of a session.
    pub fn session(&self, id: SessionId) -> Option<SessionSummary> {
        self.lock().sessions.iter().find(|s| s.id == id).cloned()
    }
}

/// Records a change and sends it to the subscribed controller.
fn publish(st: &mut State, event: SessionsEvent) {
    st.rev += 1;
    if let (Some(peer), Some(sub)) = (&st.peer, st.sessions_sub) {
        peer.notify::<SessionsEvents>(SessionsEventsParams {
            subscription_id: sub,
            changes: vec![SessionsChange {
                rev: Rev(st.rev),
                event,
            }],
        });
    }
}

/// A new idle session on `workspaces` (all connected), created now.
pub fn session_summary(
    id: SessionId,
    workspaces: &[controlroom_protocol::session::WorkspaceSpec],
) -> SessionSummary {
    let now = UnixMillis::now();
    let mut s: SessionSummary = serde_json::from_value(json!({
        "id": id, "name": null, "title": null, "workspaces": [], "shells": [],
        "created_at": now, "updated_at": now, "closed_at": null, "parent_id": null,
        "state": "idle", "last_outcome": null, "last_error": null, "read_seq": 0,
        "last_turn_end_seq": 0, "unread": false, "preview": "", "attention": null,
        "rollup": {"approvals": 0, "questions": 0, "problems": 0, "unread": 0},
        "context": null,
        "queue": {"len": 0, "paused": false, "auto_send": true, "send_now_pending": false},
        "requests": [], "todo": null,
        "live_work": {"jobs": 0, "children": 0, "asks": 0, "host_calls": 0},
        "options": [], "agent_live": true,
        "sandbox": {"level": "strict", "default_level": "strict", "explicit": false,
                    "looser_than_default": false, "grants": []},
        "resumable": true
    }))
    .unwrap();
    s.workspaces = workspaces
        .iter()
        .enumerate()
        .map(|(i, w)| Workspace {
            id: WorkspaceId::generate(),
            machine_id: w.machine_id,
            path: w.path.clone(),
            root: None,
            primary: i == 0,
            added_at: now,
            availability: Availability {
                connectivity: Connectivity::Connected,
                since: now,
            },
        })
        .collect();
    s
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, RpcError> {
    Ok(serde_json::to_value(v).unwrap())
}

fn handle(
    state: &Mutex<State>,
    peer: Peer,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let mut st = state.lock().unwrap();
    st.calls.push((method.to_owned(), params.clone()));
    match method {
        "hello" => {
            st.hellos += 1;
            to_value(&HelloResult {
                daemon_version: "0.0.0-fake".into(),
                protocol_version: AGENTD_PROTOCOL_VERSION,
                daemon_id: DaemonId::generate(),
                capabilities: vec![],
            })
        }
        "sessions.subscribe" => {
            let p: SessionsSubscribeParams = serde_json::from_value(params).unwrap();
            let sub = SubscriptionId::generate();
            st.sessions_sub = Some(sub);
            let snapshot = (p.since_rev != Some(Rev(st.rev))).then(|| SessionsSnapshot {
                sessions: st.sessions.clone(),
                settings: DaemonSettings {
                    idle_timeout_h: 48,
                    retention: RetentionPolicy {
                        delete_closed_after_days: None,
                    },
                },
            });
            to_value(&SessionsSubscribeResult {
                subscription_id: sub,
                rev: Rev(st.rev),
                snapshot,
            })
        }
        "hostops.subscribe" => {
            let sub = SubscriptionId::generate();
            st.hostops_sub = Some(sub);
            st.peer = Some(peer);
            to_value(&HostOpsSubscribeResult {
                subscription_id: sub,
                requests: st.pending.clone(),
            })
        }
        "hostops.respond" => {
            let p: HostOpsRespondParams = serde_json::from_value(params).unwrap();
            // The first answer wins; the request is no longer pending.
            st.pending.retain(|r| r.id != p.id);
            st.responses.push(p);
            Ok(json!({}))
        }
        "hostops.notify" => {
            st.notifies.push(serde_json::from_value(params).unwrap());
            Ok(json!({}))
        }
        "hostops.machines" => {
            let p: controlroom_protocol::agentd::HostOpsMachinesParams =
                serde_json::from_value(params).unwrap();
            st.machines.push(p.machines);
            Ok(json!({}))
        }
        "session.create" => {
            let p: SessionCreateParams = serde_json::from_value(params).unwrap();
            if let Some(done) = st.created.get(&p.op_id.0) {
                return to_value(done);
            }
            let session = session_summary(SessionId::generate(), &p.workspaces);
            let result = SessionCreateResult {
                session: session.clone(),
                prompt: p.prompt.as_ref().map(|_| PromptResult::Queued {
                    queue_id: controlroom_protocol::ids::QueueId::generate(),
                }),
            };
            st.created.insert(p.op_id.0.clone(), result.clone());
            st.sessions.push(session.clone());
            publish(
                &mut st,
                SessionsEvent::SessionUpserted {
                    session: Box::new(session),
                },
            );
            to_value(&result)
        }
        "session.subscribe" => {
            let id: SessionId = serde_json::from_value(params["id"].clone()).unwrap();
            let sub = SubscriptionId::generate();
            st.session_subs.insert(id, sub);
            st.peer = Some(peer);
            Ok(json!({
            "subscription_id": sub, "head_seq": 0,
            "detail": {"agent_sessions": [], "queue": [], "todos": [], "jobs": [], "peers": [],
                       "auto_continue": {"status": "off", "reason": null, "runs_used": 0, "budget": 0}},
            }))
        }
        "session.prompt" => prompt(&mut st, params),
        "doctor.report" | "agents.list" => Err(RpcError::new(ErrorData::Unsupported, "fake")),
        _ => Ok(json!({})),
    }
}

/// `session.prompt`: once per op id (a repeat returns the first result); a working session
/// skips or queues by `if_busy`; a closed one refuses; an unknown one is `unknown_session`.
fn prompt(st: &mut State, params: Value) -> Result<Value, RpcError> {
    let op = params["op_id"].as_str().unwrap_or_default().to_owned();
    if let Some((_, _, done)) = st.prompts.iter().find(|(o, _, _)| *o == op) {
        return Ok(done.clone());
    }
    let id: SessionId = serde_json::from_value(params["id"].clone()).unwrap();
    let Some(session) = st.sessions.iter().find(|s| s.id == id) else {
        return Err(RpcError::new(
            ErrorData::UnknownSession { id },
            "no such session",
        ));
    };
    let result = match session.state {
        SessionState::Closed => {
            return Err(RpcError::new(
                ErrorData::InvalidState {
                    state: SessionState::Closed,
                },
                "closed",
            ));
        }
        SessionState::Working if params["if_busy"] == "skip" => json!({"outcome": "skipped"}),
        SessionState::Working => {
            json!({"outcome": "queued", "queue_id": controlroom_protocol::ids::QueueId::generate()})
        }
        _ => json!({"outcome": "sent"}),
    };
    st.prompts.push((op, params, result.clone()));
    Ok(result)
}
