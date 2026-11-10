//! A fake controller for the end-to-end tests: it says hello in the controller role, reports
//! one connected machine, `devbox`, subscribes to the host bridge and answers every host
//! request itself, as a minimal host daemon would: workspaces resolve to their path, shells
//! open, `bash` runs the command with `/bin/sh` in the workspace, `read` and `write` use the
//! real files, `job_run` accepts the job (its event log stays empty), approvals are empty.
//! Every handled method is logged.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{
    ApprovalsSubscribeResult, HostOp, HostRequest, HostResult, HostShell, JobsSubscribeResult,
    ReadContent, ShellListResult, ToolCancelResult, WorkspaceOpenResult,
};
use controlroom_protocol::ids::{RemotePath, Rev, ShellId, SubscriptionId};
use controlroom_protocol::jsonrpc::Ack;
use controlroom_protocol::session::{Shell, ShellState};
use controlroom_protocol::{ErrorData, Resource, RpcError, UnixMillis};
use serde::Serialize;
use serde_json::{Value, json};

use crate::harness::{Client, hello_params};

/// The controller's id (a reconnect replaces its old connection).
pub const CONTROLLER: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";
/// The machine's host epoch (the same "host daemon" across agent daemon restarts).
const EPOCH: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W4";

/// The fake controller's connection and responder thread.
pub struct FakeController {
    client: Arc<Client>,
    thread: Option<JoinHandle<()>>,
    methods: Arc<Mutex<Vec<String>>>,
}

impl FakeController {
    /// Connects to the agent daemon at `socket` and starts serving `machine`; files of
    /// `home` play the host's home.
    pub fn connect(socket: &Path, machine: &str, home: &Path) -> Self {
        let client = Arc::new(Client::connect(socket));
        client.call(
            "hello",
            hello_params(json!({"kind": "controller", "controller_id": CONTROLLER})),
        );
        client.call(
            "hostops.machines",
            json!({"machines": [{
                "machine_id": machine,
                "name": "devbox",
                "availability": {"connectivity": {"state": "connected"}, "since": UnixMillis::now()},
                "epoch": EPOCH,
                "grace_s": 600,
            }]}),
        );
        let methods = Arc::new(Mutex::new(Vec::new()));
        let pending = client.call("hostops.subscribe", json!({}));
        let host = Host {
            client: client.clone(),
            home: home.to_owned(),
            methods: methods.clone(),
        };
        let thread = std::thread::spawn(move || {
            host.answer_all(&pending["requests"]);
            while let Some(n) = host
                .client
                .next_notification(|n| n["method"] == "hostops.request")
            {
                host.answer_all(&n["params"]["requests"]);
            }
        });
        FakeController {
            client,
            thread: Some(thread),
            methods,
        }
    }

    /// Waits until the agent daemon closes this controller's connection.
    pub fn wait_closed(&self) {
        self.client.wait_closed();
    }

    /// The host methods answered so far, in order.
    pub fn methods(&self) -> Vec<String> {
        self.methods
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeController {
    fn drop(&mut self) {
        // The responder ends once the connection is closed.
        self.client.shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Host {
    client: Arc<Client>,
    home: PathBuf,
    methods: Arc<Mutex<Vec<String>>>,
}

fn ok<T: Serialize>(value: T) -> Result<Value, RpcError> {
    Ok(serde_json::to_value(value).expect("serializable"))
}

fn fail(data: ErrorData, message: &str) -> Result<Value, RpcError> {
    Err(RpcError::new(data, message))
}

impl Host {
    fn answer_all(&self, requests: &Value) {
        for raw in requests.as_array().into_iter().flatten() {
            let request: HostOpRequest = serde_json::from_value(raw.clone()).expect("a request");
            self.methods
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request.request.method().to_owned());
            let outcome = match self.handle(&request) {
                Ok(result) => json!({"id": request.id, "result": result}),
                Err(error) => json!({"id": request.id, "error": error}),
            };
            // Fire and forget: the agent daemon acks every response.
            self.client.send("hostops.respond", outcome);
        }
    }

    fn handle(&self, request: &HostOpRequest) -> Result<Value, RpcError> {
        match &request.request {
            HostRequest::WorkspaceOpen(p) => ok(WorkspaceOpenResult {
                root: p.path.clone(),
                home: RemotePath::new(self.home.to_string_lossy()),
                context_files: Vec::new(),
                skills: Vec::new(),
            }),
            HostRequest::ShellOpen(p) => {
                let session = request.session_id.expect("a session's shell");
                ok(HostShell {
                    shell_id: ShellId::of(session, &p.name),
                    shell: Shell {
                        name: p.name.clone(),
                        workspace_id: p.workspace_id,
                        owner: p.owner,
                        sandbox: p.sandbox,
                        cwd: p.cwd.clone().unwrap_or_else(|| RemotePath::new("/")),
                        state: ShellState::Idle,
                        reset_pending: false,
                        foreground: None,
                    },
                })
            }
            HostRequest::ShellList(_) => ok(ShellListResult { shells: Vec::new() }),
            HostRequest::JobsSubscribe(_) => ok(JobsSubscribeResult {
                subscription_id: SubscriptionId::generate(),
                rev: Rev(0),
                events: Vec::new(),
            }),
            HostRequest::ApprovalsSubscribe(_) => ok(ApprovalsSubscribeResult {
                subscription_id: SubscriptionId::generate(),
                pending: Vec::new(),
            }),
            HostRequest::ToolRun(p) => run(&p.op),
            HostRequest::ToolAttach(p) => fail(
                ErrorData::NotFound {
                    resource: Resource::Call,
                    id: p.call_id.to_string(),
                },
                "never started",
            ),
            HostRequest::ToolCancel(_) => ok(ToolCancelResult { cancelled: false }),
            HostRequest::WorkspaceClose(_)
            | HostRequest::ShellClose(_)
            | HostRequest::JournalAck(_)
            | HostRequest::JobsAck(_)
            | HostRequest::ApprovalRespond(_)
            | HostRequest::Unsubscribe(_) => ok(Ack {}),
            _ => fail(ErrorData::Unsupported, "not in the fake host"),
        }
    }
}

/// Runs a tool op on the local files.
fn run(op: &HostOp) -> Result<Value, RpcError> {
    match op {
        HostOp::Bash { command, .. } => {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .output()
                .expect("run /bin/sh");
            ok(HostResult::Bash {
                exit_code: out.status.code(),
                signal: None,
                cwd: RemotePath::new("/"),
                output: String::from_utf8_lossy(&out.stdout).into_owned(),
                truncated: false,
                log_path: RemotePath::new("/dev/null"),
                timed_out: false,
                cancelled: false,
                waited_for_input: false,
                shell_reset: None,
                duration_ms: 1,
            })
        }
        HostOp::Read { path, .. } => {
            let text = std::fs::read_to_string(path.as_str()).map_err(|e| {
                RpcError::new(
                    ErrorData::FsError {
                        path: path.clone(),
                        detail: e.to_string(),
                    },
                    "unreadable",
                )
            })?;
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            ok(HostResult::Read {
                path: path.clone(),
                size: text.len() as u64,
                content: ReadContent::Text {
                    total_lines: lines.len() as u64,
                    lines,
                    start_line: 1,
                    cut: None,
                    first_line_too_long: false,
                },
            })
        }
        HostOp::JobRun { command, name, .. } => {
            let id = controlroom_protocol::ids::JobId::generate();
            ok(HostResult::JobRun {
                job: Box::new(controlroom_protocol::session::Job {
                    id,
                    name: name.clone().unwrap_or_else(|| command.clone()),
                    command: command.clone(),
                    cwd: RemotePath::new("/"),
                    machine_id: None,
                    workspace_id: None,
                    status: controlroom_protocol::session::JobStatus::Running,
                    exit: None,
                    signal: None,
                    started_at: UnixMillis::now(),
                    ended_at: None,
                    timeout_s: None,
                    log_path: RemotePath::new(format!("/tmp/{id}.log")),
                    lines: 0,
                    follow: None,
                }),
            })
        }
        HostOp::Write { path, content, .. } => {
            std::fs::write(path.as_str(), content).expect("write");
            ok(HostResult::Write {
                path: path.clone(),
                bytes: content.len() as u64,
                sha256: "0".repeat(64),
            })
        }
        _ => fail(ErrorData::Unsupported, "not in the fake host"),
    }
}
