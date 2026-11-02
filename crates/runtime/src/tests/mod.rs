//! Runtime-level scenario tests: the prototype's daemon e2e scenarios, rewritten against the
//! runtime with the fake driver and an in-memory host. Ordered by gates and feed events; there
//! are no sleeps (timeouts only turn a hang into a failure).

mod host;
mod lifecycle;
mod queue;
mod recovery;
mod requests;
mod sessions;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::FakeScript;
use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::{
    IfBusy, MachineChange, MachineEvent, PromptResult, SessionCreateParams, SessionOpParams,
    SessionPromptParams,
};
use controlroom_protocol::ids::{OpId, RemotePath, SessionId};
use controlroom_protocol::session::{ContentPart, PromptOrigin, SessionState, SessionSummary};
use tempfile::TempDir;
use tokio::sync::broadcast;

pub(crate) use host::{FakeAgents, FakeHost, ManualClock};

use crate::{Runtime, RuntimeConfig, SessionUpdate};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A runtime on a temp store with the in-memory host.
pub(crate) struct Harness {
    pub rt: Runtime,
    pub host: FakeHost,
    pub clock: Arc<ManualClock>,
    capabilities: Capabilities,
    dir: TempDir,
    machine: broadcast::Receiver<MachineChange>,
    sessions: HashMap<SessionId, broadcast::Receiver<SessionUpdate>>,
    /// Every summary the machine feed carried, in order.
    pub emitted: Vec<SessionSummary>,
}

/// The fake driver's default capabilities: no steering, resumable.
pub(crate) fn caps() -> Capabilities {
    Capabilities {
        resume: true,
        ..Capabilities::default()
    }
}

impl Harness {
    pub(crate) async fn new(script: FakeScript) -> Harness {
        Harness::with(script, caps()).await
    }

    pub(crate) async fn with(script: FakeScript, capabilities: Capabilities) -> Harness {
        let clock = Arc::new(ManualClock::default());
        let host = FakeHost::new(script, clock.clone());
        let dir = tempfile::tempdir().unwrap();
        Harness::open(dir, host, clock, capabilities).await
    }

    async fn open(
        dir: TempDir,
        host: FakeHost,
        clock: Arc<ManualClock>,
        capabilities: Capabilities,
    ) -> Harness {
        let rt = Runtime::open(RuntimeConfig {
            db_path: dir.path().join("daemon.sqlite"),
            host: Arc::new(host.clone()),
            agents: Arc::new(FakeAgents { capabilities }),
            clock: clock.clone(),
        })
        .unwrap();
        let machine = rt.subscribe_machine(None).unwrap().changes;
        rt.recover().await;
        Harness {
            rt,
            host,
            clock,
            capabilities,
            dir,
            machine,
            sessions: HashMap::new(),
            emitted: Vec::new(),
        }
    }

    /// "Kills the daemon": stops the runtime (no flush, no goodbye) and opens a new one on the
    /// same store and transcripts.
    pub(crate) async fn restart(self) -> Harness {
        self.restart_after(|_| {}).await
    }

    /// Like [`Harness::restart`], doing `meanwhile` while no runtime runs.
    pub(crate) async fn restart_after(self, meanwhile: impl FnOnce(&FakeHost)) -> Harness {
        self.rt.shutdown().await;
        meanwhile(&self.host);
        Harness::open(self.dir, self.host, self.clock, self.capabilities).await
    }

    /// Creates a session and waits until its agent is idle.
    pub(crate) async fn create(&mut self) -> SessionId {
        let created = self
            .rt
            .create(SessionCreateParams {
                op_id: OpId::generate(),
                agent: "fake".into(),
                cwd: RemotePath::new("/home/alice/src/app"),
                name: None,
                parent_id: None,
                spawned_by: None,
                sandbox_level: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let id = created.session.id;
        self.wait_state(id, SessionState::Idle).await;
        id
    }

    pub(crate) async fn prompt(&self, id: SessionId, text: &str) -> PromptResult {
        self.try_prompt(id, text).await.unwrap()
    }

    pub(crate) async fn try_prompt(
        &self,
        id: SessionId,
        text: &str,
    ) -> Result<PromptResult, RpcError> {
        self.rt
            .prompt(SessionPromptParams {
                op_id: OpId::generate(),
                id,
                content: vec![ContentPart::text(text)],
                origin: PromptOrigin::default(),
                if_busy: IfBusy::Queue,
            })
            .await
    }

    pub(crate) fn summary(&self, id: SessionId) -> SessionSummary {
        self.rt.summary(id).unwrap()
    }

    /// Waits until the session's summary satisfies `pred`; wakes on every feed event.
    pub(crate) async fn wait(
        &mut self,
        id: SessionId,
        what: &str,
        pred: impl Fn(&SessionSummary) -> bool,
    ) -> SessionSummary {
        if !self.sessions.contains_key(&id) {
            let sub = self
                .rt
                .subscribe_session(id, controlroom_protocol::Seq(u64::MAX), None)
                .await
                .unwrap();
            self.sessions.insert(id, sub.updates);
        }
        let result = tokio::time::timeout(TIMEOUT, async {
            loop {
                self.drain_machine();
                let summary = self.summary(id);
                if pred(&summary) {
                    return summary;
                }
                let updates = self.sessions.get_mut(&id).unwrap();
                tokio::select! {
                    change = self.machine.recv() => {
                        if let Ok(change) = change {
                            self.record(change);
                        }
                    }
                    _ = updates.recv() => {}
                }
            }
        })
        .await;
        match result {
            Ok(summary) => summary,
            Err(_) => panic!("timed out waiting for {what}: {:#?}", self.summary(id)),
        }
    }

    pub(crate) async fn wait_state(
        &mut self,
        id: SessionId,
        state: SessionState,
    ) -> SessionSummary {
        self.wait(id, &format!("{state:?}"), |s| s.state == state)
            .await
    }

    fn drain_machine(&mut self) {
        while let Ok(change) = self.machine.try_recv() {
            self.record(change);
        }
    }

    fn record(&mut self, change: MachineChange) {
        if let MachineEvent::SessionUpserted { session } = change.event {
            self.emitted.push(*session);
        }
    }

    pub(crate) fn op(&self, id: SessionId) -> SessionOpParams {
        SessionOpParams {
            op_id: OpId::generate(),
            id,
        }
    }

    /// The session's queue detail.
    pub(crate) async fn queue(
        &self,
        id: SessionId,
    ) -> Vec<controlroom_protocol::session::QueueItem> {
        self.rt
            .subscribe_session(id, controlroom_protocol::Seq(u64::MAX), None)
            .await
            .unwrap()
            .detail
            .queue
    }
}

pub(crate) fn text(s: &str) -> Vec<ContentPart> {
    vec![ContentPart::text(s)]
}
