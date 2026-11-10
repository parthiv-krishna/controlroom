//! Fake machines: the controller's side of the host bridge and in-memory host daemons, so the
//! scenarios run the runtime's host traffic end to end without sockets.
//!
//! The fake controller pulls the runtime's host requests (`hostops.subscribe`), runs each on its
//! machine while that machine is connected, and answers (`hostops.respond`). An answer is only
//! delivered on the connection the request ran on: when the link drops first, the request stays
//! pending and is sent again after the reconnect, as the real controller does. Each host keeps
//! a journal by call id (a resend attaches or returns the result; a restart loses running
//! calls), files, named shells, a job event log, and approvals. Commands in `bash`:
//! `block <gate>` waits until the test releases the gate, `exit <n>` exits with `n`, anything
//! else prints `ok: <command>`. The machines outlive runtimes, like real hosts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use controlroom_protocol::agentd::{HostMachine, HostOpRequest};
use controlroom_protocol::host::{
    ApprovalRequestParams, ApprovalRespondParams, ApprovalsSubscribeResult, HostApproval,
    HostNotification, HostOp, HostRequest, HostResult, HostShell, JobEvent, JobEventKind,
    JobsEventParams, JobsSubscribeResult, ReadContent, ToolCancelResult, ToolOutputParams,
    ToolWaitingInputParams, WorkspaceOpenResult,
};
use controlroom_protocol::ids::{
    ApprovalId, CallId, Epoch, HostOpId, JobId, MachineId, RemotePath, Rev, SessionId, ShellId,
    SubscriptionId, UnixMillis, WorkspaceId,
};
use controlroom_protocol::jsonrpc::{Ack, RpcOutcome};
use controlroom_protocol::session::{
    Availability, Job, JobFollow, JobStatus, OptionStyle, RequestBody, RequestOption, Shell,
    ShellState,
};
use controlroom_protocol::ui::Connectivity;
use controlroom_protocol::{ErrorData, Resource, RpcError};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::AbortHandle;

use super::ManualClock;
use crate::ports::Clock;
use crate::{BridgeEvent, Runtime};

const HOME: &str = "/home/alice";

type Outcome = Result<Value, RpcError>;

/// A call in a host's journal.
#[derive(Clone)]
enum Entry {
    Running(watch::Receiver<Option<Outcome>>),
    Done(Outcome),
    /// The daemon restarted while it ran.
    Lost,
}

struct FakeJob {
    job: Job,
    lines: Vec<String>,
    follow: Option<(Option<String>, u32)>,
}

struct Machine {
    id: MachineId,
    name: String,
    connected: bool,
    /// The link is dying: the runtime still sees it connected, but nothing gets through.
    stalled: bool,
    since: UnixMillis,
    epoch: Epoch,
    grace_s: u32,
    /// Counts connections; answers go out only on the connection their request ran on.
    generation: u64,
    workspaces: BTreeMap<WorkspaceId, (SessionId, RemotePath)>,
    shells: BTreeMap<ShellId, HostShell>,
    files: BTreeMap<String, String>,
    journal: BTreeMap<CallId, Entry>,
    /// Bash commands as they started, in order.
    ran: Vec<String>,
    jobs: BTreeMap<JobId, FakeJob>,
    events: Vec<JobEvent>,
    jobs_subscription: Option<SubscriptionId>,
    approvals: Vec<HostApproval>,
    approvals_subscription: Option<SubscriptionId>,
    /// Every request method the host received, in order.
    methods: Vec<String>,
    acked_calls: Vec<CallId>,
    acked_rev: Option<Rev>,
    answers: Vec<ApprovalRespondParams>,
}

impl Machine {
    fn root(path: &RemotePath) -> RemotePath {
        match path.as_str().strip_prefix('~') {
            Some(rest) => RemotePath::new(format!("{HOME}{rest}")),
            None => path.clone(),
        }
    }

    fn report(&self) -> HostMachine {
        HostMachine {
            machine_id: self.id,
            name: self.name.clone(),
            availability: Availability {
                connectivity: if self.connected {
                    Connectivity::Connected
                } else {
                    Connectivity::Unreachable
                },
                since: self.since,
            },
            epoch: Some(self.epoch),
            grace_s: self.grace_s,
        }
    }
}

#[derive(Default)]
struct State {
    machines: BTreeMap<MachineId, Machine>,
    rt: Option<Runtime>,
    controller: Option<AbortHandle>,
    /// The controller's pending requests, with the connection each is running on.
    pending: BTreeMap<HostOpId, (HostOpRequest, Option<u64>)>,
    /// Released `block` gates.
    gates: Vec<String>,
}

/// The fake machines.
#[derive(Clone)]
pub(crate) struct FakeMachines {
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
    clock: Arc<ManualClock>,
}

fn ok<T: Serialize>(value: T) -> Outcome {
    serde_json::to_value(value).map_err(|e| RpcError::new(ErrorData::InternalError, e.to_string()))
}

fn error(data: ErrorData, message: &str) -> Outcome {
    Err(RpcError::new(data, message))
}

impl FakeMachines {
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        FakeMachines {
            state: Arc::new(Mutex::new(State::default())),
            changed: Arc::new(watch::channel(0).0),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// Adds a connected machine (reported to the runtime).
    pub(crate) fn add(&self, name: &str) -> MachineId {
        let id = MachineId::generate();
        let now = self.clock.now();
        self.lock().machines.insert(
            id,
            Machine {
                id,
                name: name.into(),
                connected: true,
                stalled: false,
                since: now,
                epoch: Epoch::generate(),
                grace_s: 600,
                generation: 1,
                workspaces: BTreeMap::new(),
                shells: BTreeMap::new(),
                files: BTreeMap::new(),
                journal: BTreeMap::new(),
                ran: Vec::new(),
                jobs: BTreeMap::new(),
                events: Vec::new(),
                jobs_subscription: None,
                approvals: Vec::new(),
                approvals_subscription: None,
                methods: Vec::new(),
                acked_calls: Vec::new(),
                acked_rev: None,
                answers: Vec::new(),
            },
        );
        self.report();
        id
    }

    /// Serves `rt` as its controller (replacing the previous runtime's link).
    pub(crate) fn attach(&self, rt: Runtime) {
        {
            let mut state = self.lock();
            if let Some(old) = state.controller.take() {
                old.abort();
            }
            state.pending.clear();
            state.rt = Some(rt.clone());
        }
        self.report();
        let this = self.clone();
        let task = tokio::spawn(async move { this.controller(rt).await });
        self.lock().controller = Some(task.abort_handle());
    }

    /// Stops serving (the runtime is going away).
    pub(crate) fn detach(&self) {
        let mut state = self.lock();
        if let Some(task) = state.controller.take() {
            task.abort();
        }
        state.rt = None;
    }

    fn report(&self) {
        let (rt, machines) = {
            let state = self.lock();
            let machines: Vec<HostMachine> = state.machines.values().map(Machine::report).collect();
            (state.rt.clone(), machines)
        };
        if let Some(rt) = rt {
            rt.hostops_machines(machines).unwrap();
        }
        self.bump();
    }

    async fn controller(self, rt: Runtime) {
        let mut subscription = rt.hostops_subscribe();
        {
            let mut state = self.lock();
            for request in subscription.requests.drain(..) {
                state.pending.insert(request.id.clone(), (request, None));
            }
        }
        let mut changed = self.changed.subscribe();
        loop {
            self.dispatch();
            tokio::select! {
                event = subscription.events.recv() => match event {
                    Ok(BridgeEvent::Request(request)) => {
                        let request = *request;
                        self.lock().pending.insert(request.id.clone(), (request, None));
                    }
                    Ok(BridgeEvent::Withdraw(id)) => {
                        self.lock().pending.remove(&id);
                    }
                    Err(_) => return,
                },
                _ = changed.changed() => {}
            }
        }
    }

    /// Runs every pending request of a connected machine not yet running on its connection.
    fn dispatch(&self) {
        let mut due = Vec::new();
        {
            let mut state = self.lock();
            let generations: BTreeMap<MachineId, u64> = state
                .machines
                .values()
                .filter(|m| m.connected && !m.stalled)
                .map(|m| (m.id, m.generation))
                .collect();
            for (request, running) in state.pending.values_mut() {
                let Some(generation) = generations.get(&request.machine_id) else {
                    continue;
                };
                if *running != Some(*generation) {
                    *running = Some(*generation);
                    due.push((request.clone(), *generation));
                }
            }
        }
        for (request, generation) in due {
            let this = self.clone();
            tokio::spawn(async move {
                let outcome = this.handle(&request).await;
                this.answer(&request, generation, outcome);
            });
        }
    }

    /// Delivers an answer on the connection its request ran on.
    fn answer(&self, request: &HostOpRequest, generation: u64, outcome: Outcome) {
        let rt = {
            let mut state = self.lock();
            let live = state
                .machines
                .get(&request.machine_id)
                .is_some_and(|m| m.connected && m.generation == generation);
            if !live || state.pending.remove(&request.id).is_none() {
                return;
            }
            state.rt.clone()
        };
        if let Some(rt) = rt {
            let outcome = match outcome {
                Ok(value) => RpcOutcome::Result(value),
                Err(e) => RpcOutcome::Error(e),
            };
            rt.hostops_respond(&request.id, outcome);
        }
    }

    async fn handle(&self, request: &HostOpRequest) -> Outcome {
        let machine = request.machine_id;
        if let HostRequest::ToolRun(params) = &request.request {
            return self.tool_run(machine, request.session_id, params).await;
        }
        if let HostRequest::ToolAttach(params) = &request.request {
            let entry = self.with(machine, |m| m.journal.get(&params.call_id).cloned());
            return match entry {
                Some(Entry::Done(outcome)) => outcome,
                Some(Entry::Running(rx)) => wait(rx).await,
                Some(Entry::Lost) => error(ErrorData::Lost { may_have_run: true }, "lost"),
                None => error(
                    ErrorData::NotFound {
                        resource: Resource::Call,
                        id: params.call_id.to_string(),
                    },
                    "never started",
                ),
            };
        }
        let now = self.clock.now();
        self.with(machine, |m| {
            m.methods.push(request.request.method().to_owned());
            match &request.request {
                HostRequest::WorkspaceOpen(p) => {
                    if p.path.as_str().contains("missing") {
                        return error(
                            ErrorData::FsError {
                                path: p.path.clone(),
                                detail: "no such directory".into(),
                            },
                            "no such directory",
                        );
                    }
                    let root = Machine::root(&p.path);
                    m.workspaces
                        .insert(p.workspace_id, (p.session_id, root.clone()));
                    ok(WorkspaceOpenResult {
                        root: root.clone(),
                        home: RemotePath::new(HOME),
                        context_files: vec![controlroom_protocol::host::ContextFile {
                            path: RemotePath::new(format!("{root}/AGENTS.md")),
                            content: format!("rules of {}", m.name),
                        }],
                        skills: Vec::new(),
                    })
                }
                HostRequest::WorkspaceClose(p) => {
                    m.workspaces.remove(&p.workspace_id);
                    ok(Ack {})
                }
                HostRequest::ShellOpen(p) => {
                    let Some((session, root)) = m.workspaces.get(&p.workspace_id).cloned() else {
                        return error(
                            ErrorData::NotFound {
                                resource: Resource::Workspace,
                                id: p.workspace_id.to_string(),
                            },
                            "no workspace",
                        );
                    };
                    let shell_id = ShellId::of(session, &p.name);
                    let shell = m
                        .shells
                        .entry(shell_id.clone())
                        .or_insert_with(|| HostShell {
                            shell_id,
                            shell: Shell {
                                name: p.name.clone(),
                                workspace_id: p.workspace_id,
                                owner: p.owner,
                                sandbox: p.sandbox,
                                cwd: p.cwd.clone().unwrap_or(root),
                                state: ShellState::Idle,
                                reset_pending: false,
                                foreground: None,
                            },
                        });
                    ok(shell.clone())
                }
                HostRequest::ShellClose(p) => {
                    m.shells.remove(&p.shell_id);
                    ok(Ack {})
                }
                HostRequest::ShellList(_) => ok(controlroom_protocol::host::ShellListResult {
                    shells: m.shells.values().cloned().collect(),
                }),
                HostRequest::ToolCancel(p) => {
                    let running = matches!(m.journal.get(&p.call_id), Some(Entry::Running(_)));
                    ok(ToolCancelResult { cancelled: running })
                }
                HostRequest::JournalAck(p) => {
                    m.acked_calls.extend(p.call_ids.iter().cloned());
                    ok(Ack {})
                }
                HostRequest::JobsSubscribe(p) => {
                    let subscription = SubscriptionId::generate();
                    m.jobs_subscription = Some(subscription);
                    let events = m
                        .events
                        .iter()
                        .filter(|e| p.since.is_none_or(|since| e.rev > since))
                        .cloned()
                        .collect();
                    ok(JobsSubscribeResult {
                        subscription_id: subscription,
                        rev: m.events.last().map_or(Rev(0), |e| e.rev),
                        events,
                    })
                }
                HostRequest::JobsAck(p) => {
                    m.acked_rev = Some(p.rev);
                    ok(Ack {})
                }
                HostRequest::ApprovalsSubscribe(_) => {
                    let subscription = SubscriptionId::generate();
                    m.approvals_subscription = Some(subscription);
                    ok(ApprovalsSubscribeResult {
                        subscription_id: subscription,
                        pending: m.approvals.clone(),
                    })
                }
                HostRequest::ApprovalRespond(p) => {
                    m.approvals.retain(|a| a.id != p.approval_id);
                    m.answers.push(p.clone());
                    ok(Ack {})
                }
                _ => {
                    let _ = now;
                    error(ErrorData::Unsupported, "not in the fake host")
                }
            }
        })
    }

    async fn tool_run(
        &self,
        machine: MachineId,
        session: Option<SessionId>,
        params: &controlroom_protocol::host::ToolRunParams,
    ) -> Outcome {
        let call = params.call_id.clone();
        let entry = self.with(machine, |m| {
            m.methods.push("tool.run".into());
            m.journal.get(&call).cloned()
        });
        match entry {
            Some(Entry::Done(outcome)) => return outcome,
            Some(Entry::Running(rx)) => return wait(rx).await,
            Some(Entry::Lost) => return error(ErrorData::Lost { may_have_run: true }, "lost"),
            None => {}
        }
        let (tx, rx) = watch::channel(None);
        self.with(machine, |m| {
            m.journal.insert(call.clone(), Entry::Running(rx.clone()))
        });
        let outcome = self.run_op(machine, session, params).await;
        let epoch_alive = self.with(machine, |m| {
            if matches!(m.journal.get(&call), Some(Entry::Running(_))) {
                m.journal.insert(call.clone(), Entry::Done(outcome.clone()));
                true
            } else {
                false
            }
        });
        if !epoch_alive {
            // The daemon restarted meanwhile: whoever asks again gets `lost`.
            return error(ErrorData::Lost { may_have_run: true }, "lost");
        }
        tx.send_replace(Some(outcome.clone()));
        outcome
    }

    async fn run_op(
        &self,
        machine: MachineId,
        session: Option<SessionId>,
        params: &controlroom_protocol::host::ToolRunParams,
    ) -> Outcome {
        let now = self.clock.now();
        let root = self.with(machine, |m| {
            m.workspaces
                .get(&params.workspace_id)
                .map(|(_, root)| root.clone())
        });
        let Some(root) = root else {
            return error(
                ErrorData::NotFound {
                    resource: Resource::Workspace,
                    id: params.workspace_id.to_string(),
                },
                "no workspace",
            );
        };
        let resolve = |path: &RemotePath| -> String {
            let path = Machine::root(path);
            if path.as_str().starts_with('/') {
                path.to_string()
            } else {
                format!("{root}/{path}")
            }
        };
        match &params.op {
            HostOp::Read { path, .. } => {
                let path = resolve(path);
                let Some(content) = self.with(machine, |m| m.files.get(&path).cloned()) else {
                    return error(
                        ErrorData::FsError {
                            path: RemotePath::new(path.clone()),
                            detail: "missing".into(),
                        },
                        "missing",
                    );
                };
                let lines: Vec<String> = content.lines().map(str::to_owned).collect();
                ok(HostResult::Read {
                    path: RemotePath::new(path),
                    size: content.len() as u64,
                    content: ReadContent::Text {
                        total_lines: lines.len() as u64,
                        lines,
                        start_line: 1,
                        cut: None,
                        first_line_too_long: false,
                    },
                })
            }
            HostOp::Write { path, content, .. } => {
                let path = resolve(path);
                self.with(machine, |m| m.files.insert(path.clone(), content.clone()));
                ok(HostResult::Write {
                    path: RemotePath::new(path),
                    bytes: content.len() as u64,
                    sha256: "0".repeat(64),
                })
            }
            HostOp::Bash { shell, command, .. } => {
                self.with(machine, |m| m.ran.push(command.clone()));
                let mut output = format!("ok: {command}\n");
                let mut exit_code = 0;
                if let Some(gate) = command.strip_prefix("block ") {
                    let mut changed = self.changed.subscribe();
                    loop {
                        if self.lock().gates.iter().any(|g| g == gate) {
                            break;
                        }
                        if changed.changed().await.is_err() {
                            break;
                        }
                    }
                    output = format!("ran {gate}\n");
                } else if let Some(code) = command.strip_prefix("exit ") {
                    exit_code = code.trim().parse().unwrap_or(1);
                    output = String::new();
                }
                let cwd = session
                    .and_then(|s| {
                        self.with(machine, |m| {
                            m.shells
                                .get(&ShellId::of(s, shell))
                                .map(|sh| sh.shell.cwd.clone())
                        })
                    })
                    .unwrap_or(root);
                ok(HostResult::Bash {
                    exit_code: Some(exit_code),
                    signal: None,
                    cwd,
                    output,
                    truncated: false,
                    log_path: RemotePath::new("/tmp/bash.log"),
                    timed_out: false,
                    cancelled: false,
                    waited_for_input: false,
                    shell_reset: None,
                    duration_ms: 1,
                })
            }
            HostOp::JobRun {
                command,
                name,
                timeout_s,
                follow,
                ..
            } => {
                let Some(session) = session else {
                    return error(ErrorData::InvalidParams, "no session");
                };
                let id = JobId::generate();
                let job = Job {
                    id,
                    name: name.clone().unwrap_or_else(|| command.clone()),
                    command: command.clone(),
                    cwd: root.clone(),
                    machine_id: Some(machine),
                    workspace_id: Some(params.workspace_id),
                    status: JobStatus::Running,
                    exit: None,
                    signal: None,
                    started_at: now,
                    ended_at: None,
                    timeout_s: *timeout_s,
                    log_path: RemotePath::new(format!("{root}/.controlroom/jobs/{id}.log")),
                    lines: 0,
                    follow: None,
                };
                self.with(machine, |m| {
                    m.jobs.insert(
                        id,
                        FakeJob {
                            job: job.clone(),
                            lines: Vec::new(),
                            follow: follow
                                .as_ref()
                                .map(|f| (f.pattern.clone(), f.buffer_seconds)),
                        },
                    );
                });
                self.log(
                    machine,
                    id,
                    JobEventKind::Started {
                        session_id: session,
                        workspace_id: params.workspace_id,
                        name: job.name.clone(),
                        command: command.clone(),
                        cwd: root.clone(),
                        log_path: job.log_path.clone(),
                        timeout_s: *timeout_s,
                        follow: follow.clone(),
                    },
                );
                ok(HostResult::JobRun { job: Box::new(job) })
            }
            HostOp::JobFollow {
                job_id,
                pattern,
                buffer_seconds,
                from,
            } => {
                let buffer = buffer_seconds.unwrap_or(15);
                let matches = self.with(machine, |m| {
                    let job = m.jobs.get_mut(job_id)?;
                    job.follow = Some((pattern.clone(), buffer));
                    job.job.follow = Some(JobFollow {
                        buffer_seconds: buffer,
                        pattern: pattern.clone(),
                        next_wake_at: None,
                    });
                    Some(matching(&job.lines, 0, pattern.as_deref()))
                });
                let Some(matches) = matches else {
                    return error(
                        ErrorData::NotFound {
                            resource: Resource::Job,
                            id: job_id.to_string(),
                        },
                        "no job",
                    );
                };
                self.log(
                    machine,
                    *job_id,
                    JobEventKind::Follow {
                        pattern: pattern.clone(),
                        buffer_seconds: buffer,
                        from: *from,
                        matches,
                    },
                );
                self.job_result(machine, job_id, |job| HostResult::JobFollow { job })
            }
            HostOp::JobUnfollow { job_id } => {
                self.with(machine, |m| {
                    if let Some(job) = m.jobs.get_mut(job_id) {
                        job.follow = None;
                        job.job.follow = None;
                    }
                });
                self.log(machine, *job_id, JobEventKind::Unfollow);
                self.job_result(machine, job_id, |job| HostResult::JobUnfollow { job })
            }
            HostOp::JobKill { job_id, by } => {
                self.log(machine, *job_id, JobEventKind::Killed { by: *by });
                self.end_job(machine, *job_id, None, Some("SIGTERM"), None);
                self.job_result(machine, job_id, |job| HostResult::JobKill { job })
            }
            _ => error(ErrorData::Unsupported, "not in the fake host"),
        }
    }

    fn job_result(
        &self,
        machine: MachineId,
        job: &JobId,
        wrap: impl FnOnce(Box<Job>) -> HostResult,
    ) -> Outcome {
        let job = self.with(machine, |m| m.jobs.get(job).map(|j| j.job.clone()));
        match job {
            Some(job) => ok(wrap(Box::new(job))),
            None => error(ErrorData::InvalidParams, "no job"),
        }
    }

    /// Appends an event to the machine's job log and streams it when subscribed.
    fn log(&self, machine: MachineId, job: JobId, kind: JobEventKind) {
        let at = self.clock.now();
        let (event, notify) = self.with(machine, |m| {
            let rev = Rev(m.events.last().map_or(1, |e| e.rev.0 + 1));
            let event = JobEvent {
                rev,
                at,
                job_id: job,
                kind,
            };
            m.events.push(event.clone());
            let notify = m.connected.then_some(m.jobs_subscription).flatten();
            (event, notify)
        });
        if let Some(subscription) = notify {
            self.notify(
                machine,
                HostNotification::JobsEvent(JobsEventParams {
                    subscription_id: subscription,
                    events: vec![event],
                }),
            );
        }
    }

    fn notify(&self, machine: MachineId, notification: HostNotification) {
        let rt = self.lock().rt.clone();
        if let Some(rt) = rt {
            rt.hostops_notify(machine, notification);
        }
    }

    fn end_job(
        &self,
        machine: MachineId,
        job: JobId,
        exit_code: Option<i32>,
        signal: Option<&str>,
        lost: Option<&str>,
    ) {
        let now = self.clock.now();
        let lines = self.with(machine, |m| {
            let fake = m.jobs.get_mut(&job)?;
            if fake.job.status != JobStatus::Running {
                return None;
            }
            fake.job.status = match (exit_code, lost) {
                (_, Some(_)) => JobStatus::Lost,
                (Some(0), _) => JobStatus::Succeeded,
                _ => JobStatus::Failed,
            };
            fake.job.exit = exit_code;
            fake.job.ended_at = Some(now);
            Some(fake.lines.len() as u64)
        });
        let Some(lines) = lines else {
            return;
        };
        self.log(
            machine,
            job,
            JobEventKind::Exit {
                exit_code,
                signal: signal.map(str::to_owned),
                timed_out: false,
                lost: lost.map(str::to_owned),
                lines,
                duration_ms: 1000,
            },
        );
    }

    fn with<R>(&self, machine: MachineId, f: impl FnOnce(&mut Machine) -> R) -> R {
        let mut state = self.lock();
        f(state.machines.get_mut(&machine).expect("no such machine"))
    }

    // ---------------------------------------------------------------------------------------
    // What the test does to the machines
    // ---------------------------------------------------------------------------------------

    /// Lets every `block <gate>` command past.
    pub(crate) fn release(&self, gate: &str) {
        self.lock().gates.push(gate.into());
        self.bump();
    }

    /// The link stops carrying anything before the controller notices: requests wait, and the
    /// runtime still sees the machine connected until [`Self::drop_link`].
    pub(crate) fn stall(&self, machine: MachineId) {
        self.with(machine, |m| m.stalled = true);
        self.bump();
    }

    /// The methods of the requests waiting for the machine.
    pub(crate) fn waiting(&self, machine: MachineId) -> Vec<String> {
        self.lock()
            .pending
            .values()
            .filter(|(r, _)| r.machine_id == machine)
            .map(|(r, _)| r.request.method().to_owned())
            .collect()
    }

    /// The link to the machine drops (the controller marks it unreachable).
    pub(crate) fn drop_link(&self, machine: MachineId) {
        let now = self.clock.now();
        self.with(machine, |m| {
            m.connected = false;
            m.since = now;
        });
        self.report();
    }

    /// The link comes back: a new connection.
    pub(crate) fn reconnect(&self, machine: MachineId) {
        let now = self.clock.now();
        self.with(machine, |m| {
            m.connected = true;
            m.stalled = false;
            m.since = now;
            m.generation += 1;
        });
        self.report();
    }

    /// A shell's process ends and the host reports it `gone` (a user's terminal is forgotten).
    pub(crate) fn end_shell(&self, machine: MachineId, shell: &ShellId) {
        let gone = self.with(machine, |m| m.shells.remove(shell));
        if let Some(mut gone) = gone {
            gone.shell.state = ShellState::Gone;
            self.notify(
                machine,
                HostNotification::ShellChanged(controlroom_protocol::host::ShellChangedParams {
                    shell: gone,
                }),
            );
        }
    }

    /// The host reboots: every shell's supervisor is gone, then the daemon restarts.
    pub(crate) fn reboot(&self, machine: MachineId) {
        self.with(machine, |m| m.shells.clear());
        self.restart(machine);
    }

    /// The host daemon restarts: running calls are lost, then the link comes back with a new
    /// epoch.
    pub(crate) fn restart(&self, machine: MachineId) {
        self.with(machine, |m| {
            for entry in m.journal.values_mut() {
                if matches!(entry, Entry::Running(_)) {
                    *entry = Entry::Lost;
                }
            }
            m.epoch = Epoch::generate();
            m.jobs_subscription = None;
            m.approvals_subscription = None;
        });
        self.reconnect(machine);
    }

    /// Sets the machine's grace period.
    pub(crate) fn set_grace(&self, machine: MachineId, grace_s: u32) {
        self.with(machine, |m| m.grace_s = grace_s);
        self.report();
    }

    /// Bash commands the machine ran, in order.
    pub(crate) fn ran(&self, machine: MachineId) -> Vec<String> {
        self.with(machine, |m| m.ran.clone())
    }

    /// Request methods the machine received, in order.
    pub(crate) fn methods(&self, machine: MachineId) -> Vec<String> {
        self.with(machine, |m| m.methods.clone())
    }

    /// Calls the agent daemon released with `journal.ack`.
    pub(crate) fn acked_calls(&self, machine: MachineId) -> Vec<CallId> {
        self.with(machine, |m| m.acked_calls.clone())
    }

    /// The job log revision the agent daemon acknowledged.
    pub(crate) fn acked_rev(&self, machine: MachineId) -> Option<Rev> {
        self.with(machine, |m| m.acked_rev)
    }

    /// A file on the machine.
    pub(crate) fn file(&self, machine: MachineId, path: &str) -> Option<String> {
        self.with(machine, |m| m.files.get(path).cloned())
    }

    /// The machine's jobs.
    pub(crate) fn job_ids(&self, machine: MachineId) -> Vec<JobId> {
        self.with(machine, |m| m.jobs.keys().copied().collect())
    }

    /// The job writes lines to its log (matches of its follow are logged with them).
    pub(crate) fn job_output(&self, machine: MachineId, job: JobId, lines: &[&str]) {
        let progress = self.with(machine, |m| {
            let fake = m.jobs.get_mut(&job)?;
            let before = fake.lines.len();
            fake.lines.extend(lines.iter().map(|l| (*l).to_owned()));
            let log = fake.lines.join("\n");
            let path = fake.job.log_path.to_string();
            fake.job.lines = fake.lines.len() as u64;
            let matches = match &fake.follow {
                Some((pattern, _)) => matching(&fake.lines, before, pattern.as_deref()),
                None => Vec::new(),
            };
            let total = fake.lines.len() as u64;
            m.files.insert(path, log);
            Some((total, matches))
        });
        if let Some((lines, matches)) = progress {
            self.log(
                machine,
                job,
                JobEventKind::Progress {
                    lines,
                    offset: lines * 10,
                    matches,
                },
            );
        }
    }

    /// The job exits with `code`.
    pub(crate) fn job_exit(&self, machine: MachineId, job: JobId, code: i32) {
        self.end_job(machine, job, Some(code), None, None);
    }

    /// The host rebooted: the job's supervisor is gone.
    pub(crate) fn job_lost(&self, machine: MachineId, job: JobId) {
        self.end_job(machine, job, None, None, Some("host rebooted"));
    }

    /// nono asks for an approval in a workspace (for the agent).
    pub(crate) fn approval(
        &self,
        machine: MachineId,
        session: SessionId,
        workspace: WorkspaceId,
    ) -> ApprovalId {
        self.approval_from(machine, session, workspace, false)
    }

    /// nono asks for an approval in a workspace, for the agent or one of the user's terminals.
    pub(crate) fn approval_from(
        &self,
        machine: MachineId,
        session: SessionId,
        workspace: WorkspaceId,
        from_user_terminal: bool,
    ) -> ApprovalId {
        let approval = HostApproval {
            id: ApprovalId::generate(),
            session_id: session,
            workspace_id: workspace,
            title: "Run git push origin main?".into(),
            detail: None,
            body: RequestBody::Command {
                command: "git push origin main".into(),
                cwd: None,
            },
            from_user_terminal,
            options: vec![
                RequestOption {
                    id: "allow-once".into(),
                    label: "Allow once".into(),
                    style: OptionStyle::Allow,
                },
                RequestOption {
                    id: "deny".into(),
                    label: "Deny".into(),
                    style: OptionStyle::Deny,
                },
            ],
            created_at: self.clock.now(),
        };
        let id = approval.id;
        let notify = self.with(machine, |m| {
            m.approvals.push(approval.clone());
            m.connected.then_some(m.approvals_subscription).flatten()
        });
        if let Some(subscription) = notify {
            self.notify(
                machine,
                HostNotification::ApprovalRequest(ApprovalRequestParams {
                    subscription_id: subscription,
                    approval,
                }),
            );
        }
        id
    }

    /// The host forgets its pending approvals (nono gave up while the link was down).
    pub(crate) fn forget_approvals(&self, machine: MachineId) {
        self.with(machine, |m| m.approvals.clear());
    }

    /// Answers given to the machine's approvals.
    pub(crate) fn answers(&self, machine: MachineId) -> Vec<ApprovalRespondParams> {
        self.with(machine, |m| m.answers.clone())
    }

    /// The machine reports output of a running call.
    pub(crate) fn output(&self, machine: MachineId, call: &str, text: &str) {
        self.notify(
            machine,
            HostNotification::ToolOutput(ToolOutputParams {
                call_id: CallId::new(call),
                text: text.into(),
            }),
        );
    }

    /// The machine reports that a call waits (or stopped waiting) for terminal input.
    pub(crate) fn waiting_input(&self, machine: MachineId, call: &str, waiting: bool) {
        self.notify(
            machine,
            HostNotification::ToolWaitingInput(ToolWaitingInputParams {
                call_id: CallId::new(call),
                waiting,
            }),
        );
    }

    /// The shells the machine has.
    pub(crate) fn shells(&self, machine: MachineId) -> Vec<HostShell> {
        self.with(machine, |m| m.shells.values().cloned().collect())
    }

    /// Waits until the machine satisfies `pred`; wakes on every change.
    pub(crate) async fn wait(&self, what: &str, pred: impl Fn(&FakeMachines) -> bool) {
        let mut changed = self.changed.subscribe();
        let waited = tokio::time::timeout(super::TIMEOUT, async {
            loop {
                if pred(self) {
                    return;
                }
                tokio::select! {
                    _ = changed.changed() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                }
            }
        })
        .await;
        assert!(waited.is_ok(), "timed out waiting for {what}");
    }
}

/// Line numbers (from 1) after `skip` that match `pattern` (every line without one).
fn matching(lines: &[String], skip: usize, pattern: Option<&str>) -> Vec<u64> {
    lines
        .iter()
        .enumerate()
        .skip(skip)
        .filter(|(_, line)| pattern.is_none_or(|p| line.contains(p)))
        .map(|(i, _)| i as u64 + 1)
        .collect()
}

async fn wait(mut rx: watch::Receiver<Option<Outcome>>) -> Outcome {
    match rx.wait_for(Option::is_some).await {
        Ok(outcome) => outcome
            .clone()
            .unwrap_or_else(|| error(ErrorData::Busy, "gone")),
        Err(_) => error(ErrorData::Lost { may_have_run: true }, "lost"),
    }
}
