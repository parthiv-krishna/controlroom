//! A scriptable stand-in for `pi --mode rpc`, for deterministic tests of
//! the supervisor, runtime and daemon.
//!
//! It speaks pi's RPC protocol as recorded in `fixtures/pi-rpc/1.2.0`:
//! the same commands, responses and events, steer/follow-up queueing,
//! aborts, retries, extension dialogs and pi-format session files with
//! resume. A [`Scenario`] scripts the model's replies and the extensions'
//! commands; [`Gates`] let a test hold the agent at named points.
//!
//! The binary takes pi's flags (unknown ones are ignored) plus:
//!
//! - `--scenario <file>` / `$CONTROLROOM_FAKE_SCENARIO`: the scenario (JSON);
//! - `--scenario-json <json>`: the scenario inline;
//! - `--gate-dir <dir>` / `$CONTROLROOM_FAKE_GATES`: the gate directory.
//!
//! Sessions go to `$PI_CODING_AGENT_DIR/sessions` (default
//! `$HOME/.pi/agent/sessions`) like pi's; `--session <file>` resumes,
//! `--session-id <id>` opens or creates, `--no-session` keeps it in memory.

mod agent;
mod commands;
mod controlroom;
pub mod gates;
mod ids;
mod out;
mod run;
pub mod scenario;
mod session;
mod wire;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal::unix::{SignalKind, signal};

pub use gates::Gates;
pub use scenario::Scenario;

use crate::agent::Agent;
use crate::session::{Session, encoded_cwd};

/// Which session to open at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionChoice {
    /// A new session (with this id, if given).
    New(Option<String>),
    /// `--session-id`: open the project session with this id, or create it.
    Id(String),
    /// `--session <file>`.
    Resume(PathBuf),
    /// `--no-session`: in memory only.
    Ephemeral,
}

/// Everything the agent needs to start.
#[derive(Debug, Clone)]
pub struct Config {
    pub scenario: Scenario,
    pub gates: Option<Gates>,
    pub session: SessionChoice,
    /// `PI_CODING_AGENT_DIR`.
    pub agent_dir: PathBuf,
    pub cwd: PathBuf,
}

/// Serve the RPC protocol on stdin/stdout until stdin closes or a
/// terminating signal arrives; returns the exit code pi would use.
pub async fn serve(config: Config) -> Result<i32> {
    let cwd = config.cwd.display().to_string();
    let project_dir = config.agent_dir.join("sessions").join(encoded_cwd(&cwd));
    let first = &config.scenario.models[0];
    let model = (first.provider.as_str(), first.id.as_str());
    let thinking = config.scenario.thinking_level.as_str();
    let (session, sessions_dir) = match &config.session {
        SessionChoice::Ephemeral => (
            Session::create(None, ids::uuid(), &cwd, None, model, thinking),
            None,
        ),
        SessionChoice::Resume(path) => (Session::load(path)?, Some(project_dir)),
        SessionChoice::Id(id) => match Session::find(&project_dir, id) {
            Some(path) => (Session::load(&path)?, Some(project_dir)),
            None => {
                eprintln!(
                    "Warning: No project session found with id '{id}'; creating a new session with that id."
                );
                let session =
                    Session::create(Some(&project_dir), id.clone(), &cwd, None, model, thinking);
                (session, Some(project_dir))
            }
        },
        SessionChoice::New(id) => {
            let id = id.clone().unwrap_or_else(ids::uuid);
            let session = Session::create(Some(&project_dir), id, &cwd, None, model, thinking);
            (session, Some(project_dir))
        }
    };
    let agent = Agent::new(
        config.scenario,
        config.gates,
        sessions_dir,
        cwd,
        session,
        out::Out::stdout(),
    );

    let mut hangup = signal(SignalKind::hangup()).context("installing SIGHUP")?;
    let mut terminate = signal(SignalKind::terminate()).context("installing SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("installing SIGINT")?;

    // Like pi, finish the extensions' start-up before serving commands; only
    // steps that wait (dialogs, gates) continue in the background.
    let ran = agent.instant_steps(&agent.scenario.session_start);
    if ran < agent.scenario.session_start.len() {
        let startup = agent.clone();
        tokio::spawn(async move {
            startup
                .side_steps(&startup.scenario.session_start[ran..])
                .await
        });
    }

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut buf = Vec::new();
    loop {
        buf.clear();
        tokio::select! {
            read = stdin.read_until(b'\n', &mut buf) => {
                if read.context("reading stdin")? == 0 {
                    return Ok(0);
                }
                handle(&agent, &buf);
            }
            _ = hangup.recv() => return Ok(128 + 1),
            _ = interrupt.recv() => return Ok(128 + 2),
            _ = terminate.recv() => return Ok(128 + 15),
        }
    }
}

/// Strip the LF (and an optional CR) and dispatch one line.
fn handle(agent: &Arc<Agent>, buf: &[u8]) {
    let line = buf.strip_suffix(b"\n").unwrap_or(buf);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().all(u8::is_ascii_whitespace) {
        return;
    }
    commands::handle_line(agent, &String::from_utf8_lossy(line));
}

/// Parse the binary's arguments (pi's flags plus the fake's own).
pub fn config_from_args(args: &[String]) -> Result<Config> {
    let mut scenario_file = std::env::var_os("CONTROLROOM_FAKE_SCENARIO").map(PathBuf::from);
    let mut scenario_json = None;
    let mut gate_dir = std::env::var_os("CONTROLROOM_FAKE_GATES").map(PathBuf::from);
    let mut session = SessionChoice::New(None);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--scenario" => scenario_file = Some(value()?.into()),
            "--scenario-json" => scenario_json = Some(value()?),
            "--gate-dir" => gate_dir = Some(value()?.into()),
            "--session" => session = SessionChoice::Resume(value()?.into()),
            "--session-id" => session = SessionChoice::Id(value()?),
            "--no-session" => session = SessionChoice::Ephemeral,
            "--mode" => {
                let mode = value()?;
                if mode != "rpc" {
                    bail!("fake-agent only speaks --mode rpc, not {mode}");
                }
            }
            // pi flags that take a value; the fake ignores them.
            "-e" | "--extension" | "--provider" | "--model" | "--name" | "-n" | "--session-dir"
            | "--thinking" | "--api-key" | "--tools" => {
                value()?;
            }
            _ => {}
        }
    }
    let scenario = match (scenario_json, scenario_file) {
        (Some(json), _) => Scenario::from_json(&json).context("parsing --scenario-json")?,
        (None, Some(path)) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Scenario::from_json(&text).with_context(|| format!("parsing {}", path.display()))?
        }
        (None, None) => Scenario::default(),
    };
    let agent_dir = match std::env::var_os("PI_CODING_AGENT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".pi/agent")
        }
    };
    Ok(Config {
        scenario,
        gates: gate_dir.map(Gates::new).transpose()?,
        session,
        agent_dir,
        cwd: std::env::current_dir().context("reading the cwd")?,
    })
}
