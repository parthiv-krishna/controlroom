//! Terminal streams end to end: the controller attaches to a real host daemon's shell over the
//! in-process SSH server (through a relay that can cut the link), with a fake agent daemon
//! publishing the session. Typing, reading, resizing, two browsers, the foreground program,
//! revocation, a link drop with resume, and the end of the shell.

mod support;

use std::time::{Duration, Instant};

use controlroom_controller::test_support::{
    FakeAgentd, Relay, RemoteHost, bundled_daemons, session_summary,
};
use controlroom_controller::{
    Authenticated, CloseReason, Controller, Identity, TerminalFrame, TerminalInput, TerminalOutput,
};
use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{
    HostOp, HostRequest, ShellOpenParams, ToolRunParams, WorkspaceOpenParams,
};
use controlroom_protocol::ids::{CallId, DeviceId, HostOpId, MachineId, SessionId, WorkspaceId};
use controlroom_protocol::session::{SandboxLevel, SandboxSpec, Shell, ShellState, WorkspaceSpec};
use controlroom_protocol::terminal::TerminalEvent;
use controlroom_protocol::{ErrorData, RpcOutcome};
use support::{Setup, add_machine_at, controller_with_agentd, ui};

const DEADLINE: Duration = Duration::from_secs(30);

/// A controller, a fake agent daemon and a real host daemon ("devbox", behind a relay) with
/// one session whose shell "devbox" is open.
struct World {
    agentd: FakeAgentd,
    setup: Setup,
    host: RemoteHost,
    relay: Relay,
    machine: MachineId,
    workspace: WorkspaceId,
    session: SessionId,
    _bundled: tempfile::TempDir,
}

impl World {
    async fn start() -> Self {
        let agentd = FakeAgentd::start().await;
        let bundled = bundled_daemons();
        let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
        let host = RemoteHost::start().await;
        let relay = Relay::start(host.port()).await;
        let mut ui = ui(&setup.controller, "laptop").await;
        agentd.wait_connected(1).await;
        let machine: MachineId = add_machine_at(&mut ui, &host, "devbox", relay.port)
            .await
            .parse()
            .unwrap();
        agentd
            .wait_machines("devbox is a connected host", |m| {
                m.iter()
                    .any(|m| m.availability.is_connected() && m.epoch.is_some())
            })
            .await;

        let session = SessionId::generate();
        let mut summary = session_summary(
            session,
            &[WorkspaceSpec {
                machine_id: machine,
                path: host.work().into(),
            }],
        );
        let workspace = summary.workspaces[0].id;
        let world = Self {
            agentd,
            setup,
            host,
            relay,
            machine,
            workspace,
            session,
            _bundled: bundled,
        };
        world
            .host_call(
                "open",
                machine,
                HostRequest::WorkspaceOpen(WorkspaceOpenParams {
                    workspace_id: workspace,
                    session_id: session,
                    path: world.host.work().into(),
                    sandbox: SandboxSpec {
                        level: SandboxLevel::Off,
                        grants: Vec::new(),
                        domains: Vec::new(),
                        mediated: Vec::new(),
                    },
                }),
            )
            .await;
        world
            .host_call(
                "shell",
                machine,
                HostRequest::ShellOpen(ShellOpenParams {
                    workspace_id: workspace,
                    name: "devbox".into(),
                    cwd: None,
                }),
            )
            .await;
        summary.shells.push(Shell {
            name: "devbox".into(),
            workspace_id: workspace,
            cwd: world.host.work().into(),
            state: ShellState::Idle,
            reset_pending: false,
            foreground: None,
        });
        world.agentd.upsert_session(summary);
        world
    }

    fn controller(&self) -> &Controller {
        &self.setup.controller
    }

    /// Runs a host request through the router, as the agent daemon does.
    async fn host_call(&self, id: &str, machine: MachineId, request: HostRequest) {
        self.agentd.request(HostOpRequest {
            id: HostOpId::new(id),
            machine_id: machine,
            session_id: Some(self.session),
            request,
        });
        match self.agentd.response(&HostOpId::new(id)).await {
            RpcOutcome::Result(_) => {}
            RpcOutcome::Error(e) => panic!("{id}: {e:?}"),
        }
    }

    /// A login of the dev user on `device`.
    fn login(&self, device: Option<&str>) -> Authenticated {
        let c = self.controller();
        let principal = c.resolve(&Identity::DevToken).unwrap();
        let login = c
            .create_login(&principal, device.map(DeviceId::new))
            .unwrap();
        c.authenticate(&login.token).unwrap()
    }

    /// Opens shell `shell`, waiting for the controller to learn about the session first.
    async fn open(&self, auth: &Authenticated, shell: &str) -> Term {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match self.controller().open_terminal(auth, self.session, shell) {
                Ok(stream) => {
                    let (input, output) = stream.split();
                    return Term {
                        input,
                        output,
                        screen: String::new(),
                        foreground: None,
                        resets: 0,
                    };
                }
                Err(e) if matches!(e.data, Some(ErrorData::UnknownSession { .. })) => {
                    assert!(Instant::now() < deadline, "the session never arrived");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("opening {shell}: {e:?}"),
            }
        }
    }
}

/// One browser's view of a terminal stream.
struct Term {
    input: TerminalInput,
    output: TerminalOutput,
    /// Output since the last reset.
    screen: String,
    foreground: Option<String>,
    /// Fresh replays after the first attach.
    resets: u32,
}

impl Term {
    /// The next frame (applied to the screen); `None` once the stream is over.
    async fn frame(&mut self) -> Option<TerminalFrame> {
        let frame = tokio::time::timeout(DEADLINE, self.output.recv())
            .await
            .unwrap_or_else(|_| panic!("no frame in time; screen: {:?}", self.screen))?;
        match &frame {
            TerminalFrame::Output(data) => self.screen.push_str(&String::from_utf8_lossy(data)),
            TerminalFrame::Event(TerminalEvent::Attached { reset: true }) => {
                self.resets += u32::from(!self.screen.is_empty());
                self.screen.clear();
            }
            TerminalFrame::Event(TerminalEvent::Foreground { name }) => {
                self.foreground.clone_from(name)
            }
            TerminalFrame::Event(_) => {}
        }
        Some(frame)
    }

    /// Reads until `pred` holds for the screen.
    async fn until(&mut self, what: &str, pred: impl Fn(&Self) -> bool) {
        while !pred(self) {
            assert!(
                self.frame().await.is_some(),
                "stream over waiting for {what}"
            );
        }
    }

    /// Reads until an event matching `pred`.
    async fn event(&mut self, what: &str, pred: impl Fn(&TerminalEvent) -> bool) -> TerminalEvent {
        loop {
            match self.frame().await {
                Some(TerminalFrame::Event(event)) if pred(&event) => return event,
                Some(_) => {}
                None => panic!(
                    "stream over waiting for {what} ({:?}); screen: {:?}",
                    self.output.close_reason(),
                    self.screen
                ),
            }
        }
    }

    async fn type_line(&self, line: &str) {
        self.input
            .send(format!("{line}\r").into_bytes())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_browsers_share_a_shell_type_resize_and_see_the_foreground() {
    let world = World::start().await;
    let auth = world.login(None);
    let mut a = world.open(&auth, "devbox").await;
    a.event("the first attach", |e| {
        *e == TerminalEvent::Attached { reset: true }
    })
    .await;
    a.type_line("echo typed-$((6*7))").await;
    a.until("the typed command's output", |t| {
        t.screen.contains("typed-42")
    })
    .await;

    // A second browser gets the replay, and typing from either reaches both.
    let mut b = world.open(&auth, "devbox").await;
    b.until("the replay", |t| t.screen.contains("typed-42"))
        .await;
    b.type_line("echo from-$((1+1))-b").await;
    a.until("b's output", |t| t.screen.contains("from-2-b"))
        .await;

    a.input.resize(101, 37).await.unwrap();
    a.type_line("stty size").await;
    b.until("the new size", |t| t.screen.contains("37 101"))
        .await;

    b.type_line("sleep 300").await;
    a.until("sleep in the foreground", |t| {
        t.foreground.as_deref() == Some("sleep")
    })
    .await;
    b.input.send(b"\x03".to_vec()).await.unwrap();
    a.until("back at the prompt", |t| t.foreground.is_none())
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_the_login_closes_its_streams() {
    let world = World::start().await;
    let c = world.controller();

    let auth = world.login(None);
    let mut term = world.open(&auth, "devbox").await;
    term.event("attached", |e| matches!(e, TerminalEvent::Attached { .. }))
        .await;
    c.logout(&auth).unwrap();
    while term.frame().await.is_some() {}
    assert_eq!(
        term.output.close_reason(),
        Some(CloseReason::Unauthenticated)
    );
    assert!(term.input.send(b"x".to_vec()).await.is_err());

    // Removing a device closes the streams of its logins; others stay open.
    let phone = world.login(Some("phone"));
    let laptop = world.login(Some("laptop"));
    let mut on_phone = world.open(&phone, "devbox").await;
    let mut on_laptop = world.open(&laptop, "devbox").await;
    on_phone
        .event("attached", |e| matches!(e, TerminalEvent::Attached { .. }))
        .await;
    on_laptop
        .event("attached", |e| matches!(e, TerminalEvent::Attached { .. }))
        .await;
    // A tab registers the device (`hello`).
    let _phone_tab = ui(c, "phone").await;
    c.revoke_device(&phone.principal.owner, &DeviceId::new("phone"))
        .unwrap_or_else(|e| panic!("{e:?}"));
    while on_phone.frame().await.is_some() {}
    assert_eq!(
        on_phone.output.close_reason(),
        Some(CloseReason::Unauthenticated)
    );
    on_laptop.type_line("echo still-$((2*2))").await;
    on_laptop
        .until("the laptop still works", |t| t.screen.contains("still-4"))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_drop_detaches_and_the_stream_resumes_without_loss() {
    let world = World::start().await;
    let auth = world.login(None);
    let mut term = world.open(&auth, "devbox").await;
    term.type_line("echo be''fore").await;
    term.until("the first output", |t| t.screen.contains("before\r\n"))
        .await;

    // Output produced while the link is down is replayed once it is back.
    let gate = std::path::PathBuf::from(world.host.work()).join("gate");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success()
    );
    term.type_line(&format!("cat {}; echo af''ter", gate.display()))
        .await;
    term.until("the command echo", |t| t.screen.contains("af''ter"))
        .await;
    world.relay.down();
    let detached = term
        .event("detached", |e| matches!(e, TerminalEvent::Detached { .. }))
        .await;
    assert!(
        matches!(&detached, TerminalEvent::Detached { reason } if reason.starts_with("devbox")),
        "{detached:?}"
    );
    tokio::task::spawn_blocking(move || std::fs::write(gate, "during-the-drop\n").unwrap())
        .await
        .unwrap();
    // Typing while detached is dropped, not an error.
    term.input.send(b"lost".to_vec()).await.unwrap();
    world.relay.up();
    let resumed = term
        .event("attached again", |e| {
            matches!(e, TerminalEvent::Attached { .. })
        })
        .await;
    assert_eq!(resumed, TerminalEvent::Attached { reset: false });
    term.until("the output from the drop", |t| {
        t.screen.contains("after\r\n")
    })
    .await;
    assert!(term.screen.contains("during-the-drop"), "{:?}", term.screen);
    assert_eq!(
        term.screen.matches("before\r\n").count(),
        1,
        "nothing is shown twice: {:?}",
        term.screen
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stream_ends_with_the_shell_and_unknown_shells_are_refused() {
    let world = World::start().await;
    let c = world.controller();
    let auth = world.login(None);
    let err = c
        .open_terminal(&auth, SessionId::generate(), "devbox")
        .err()
        .unwrap();
    assert!(
        matches!(err.data, Some(ErrorData::UnknownSession { .. })),
        "{err:?}"
    );

    let mut term = world.open(&auth, "devbox").await;
    let err = c.open_terminal(&auth, world.session, "nope").err().unwrap();
    assert!(
        matches!(err.data, Some(ErrorData::ShellGone { .. })),
        "{err:?}"
    );

    term.type_line("exit").await;
    let ended = term
        .event("ended", |e| matches!(e, TerminalEvent::Ended { .. }))
        .await;
    assert!(
        matches!(&ended, TerminalEvent::Ended { reason } if reason.contains("ended")),
        "{ended:?}"
    );
    assert!(term.frame().await.is_none());
    assert_eq!(term.output.close_reason(), None, "a normal end");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_that_falls_behind_stalls_nothing_and_loses_nothing() {
    const LINES: u32 = 50_000;
    let world = World::start().await;
    let auth = world.login(None);
    let mut slow = world.open(&auth, "devbox").await;
    slow.event("attached", |e| matches!(e, TerminalEvent::Attached { .. }))
        .await;

    // The agent floods the shared terminal while this browser reads nothing: the SSH
    // connection keeps serving host calls (the agent's result arrives).
    world
        .host_call(
            "flood",
            world.machine,
            HostRequest::ToolRun(ToolRunParams {
                call_id: CallId::new("flood"),
                workspace_id: world.workspace,
                op: HostOp::Bash {
                    shell: "devbox".into(),
                    command: format!("seq 1 {LINES}"),
                    timeout_s: None,
                },
            }),
        )
        .await;

    // Then it catches up: every line once, in order (from the start unless the host had to
    // replay afresh because the browser fell too far behind).
    let last = format!("\r\n{LINES}\r\n");
    slow.until("the last line", |t| t.screen.contains(&last))
        .await;
    let numbers: Vec<u32> = slow
        .screen
        .split("\r\n")
        // The first line follows the command's OSC 133 marker.
        .filter_map(|line| line.rsplit('\x07').next()?.parse().ok())
        .collect();
    let first = if slow.resets == 0 { 1 } else { numbers[0] };
    assert!(
        numbers.iter().copied().eq(first..=LINES),
        "{} numbers from {first} ({} resets)",
        numbers.len(),
        slow.resets
    );
}
