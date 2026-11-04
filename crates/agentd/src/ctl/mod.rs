//! `controlroomd ctl`: a debugging client for the local daemon (`hello` with the `debug` role)
//! and a terminal renderer for timeline ops.
//!
//! ```text
//! controlroomd ctl hello                         handshake; prints the daemon's hello result
//! controlroomd ctl call <method> [<params JSON>] any daemon method; prints the result
//! controlroomd ctl watch <session> [--since N]   subscribe and render the timeline live
//! controlroomd ctl render [<file>]               render ops offline (JSONL from a file or stdin)
//! controlroomd ctl create [--agent pi] [--cwd D] [--sandbox off] [prompt...]
//! controlroomd ctl prompt <session> <text...>
//! controlroomd ctl cancel <session>
//! controlroomd ctl approve <session> <request> <option>
//! ```
//!
//! `render` accepts lines that are `session.events` notifications, their params, or single
//! timeline ops, so recorded feeds and driver output can be inspected without a daemon.

pub mod client;
pub mod render;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use controlroom_protocol::daemon::notifications::SessionEvents;
use controlroom_protocol::daemon::{
    InitialPrompt, PromptResult, RequestRespondParams, SessionCreateParams, SessionEventsParams,
    SessionOpParams, SessionPromptParams, SessionSubscribeParams, methods,
};
use controlroom_protocol::ids::{OpId, RequestId, SessionId};
use controlroom_protocol::session::{ContentPart, Response, SandboxLevel};
use controlroom_protocol::timeline::TimelineOp;
use controlroom_protocol::{NotificationMethod, Seq};
use serde_json::Value;

use crate::connect::connect_or_start;
use crate::dirs::Dirs;
use client::DaemonClient;
use render::Renderer;

/// Arguments of `controlroomd ctl`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Say hello as a debug client and print the result.
    Hello,
    /// Call a daemon method and print its result as JSON.
    Call {
        /// The method, e.g. `agents.list`.
        method: String,
        /// The params as JSON [default: {}].
        params: Option<String>,
    },
    /// Subscribe to a session and render its timeline until the daemon closes the connection.
    Watch {
        /// The session id.
        session: SessionId,
        /// Render items changed after this seq (0: the whole timeline).
        #[arg(long, default_value_t = 0)]
        since: u64,
    },
    /// Create a session and print its summary; with a prompt, send it too.
    Create {
        /// The agent key in agents.toml.
        #[arg(long, default_value = "pi")]
        agent: String,
        /// The working directory [default: the current directory].
        #[arg(long)]
        cwd: Option<String>,
        /// A name for the session.
        #[arg(long)]
        name: Option<String>,
        /// The sandbox level (strict, relaxed or off) [default: the daemon's default].
        #[arg(long)]
        sandbox: Option<String>,
        /// A first prompt.
        prompt: Vec<String>,
    },
    /// Send a prompt (queued while the agent is busy).
    Prompt {
        /// The session id.
        session: SessionId,
        /// The text.
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Cancel the running turn.
    Cancel {
        /// The session id.
        session: SessionId,
    },
    /// Answer a request (approval or dialog) with one of its options.
    Approve {
        /// The session id.
        session: SessionId,
        /// The request id.
        request: String,
        /// The option id.
        option: String,
    },
    /// Render timeline ops from a JSONL file (or stdin).
    Render {
        /// The file [default: stdin].
        file: Option<PathBuf>,
    },
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let result = match args.command {
        Cmd::Render { file } => render_file(file),
        command => Dirs::from_env(args.state_dir.as_deref())
            .map_err(anyhow::Error::from)
            .and_then(|dirs| online(&dirs, command)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroomd ctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn online(dirs: &Dirs, command: Cmd) -> anyhow::Result<()> {
    let mut client = DaemonClient::new(connect_or_start(dirs)?)?;
    let hello = client.hello().context("hello")?;
    match command {
        Cmd::Hello => print_json(&serde_json::to_value(hello)?),
        Cmd::Call { method, params } => {
            let params: Value = match params {
                Some(p) => serde_json::from_str(&p).context("params are not JSON")?,
                None => Value::Object(Default::default()),
            };
            print_json(&client.call_raw(&method, params).context(method)?)
        }
        Cmd::Watch { session, since } => watch(&mut client, session, since),
        Cmd::Create {
            agent,
            cwd,
            name,
            sandbox,
            prompt,
        } => create(&mut client, agent, cwd, name, sandbox, &prompt),
        Cmd::Prompt { session, text } => {
            print_json(&serde_json::to_value(prompt(&mut client, session, &text)?)?)
        }
        Cmd::Cancel { session } => {
            client.call::<methods::SessionCancel>(SessionOpParams {
                op_id: OpId::generate(),
                id: session,
            })?;
            Ok(())
        }
        Cmd::Approve {
            session,
            request,
            option,
        } => {
            let result = client.call::<methods::RequestRespond>(RequestRespondParams {
                op_id: OpId::generate(),
                id: session,
                request_id: RequestId::new(request),
                response: Response::Choice {
                    option_id: option,
                    reason: None,
                },
                then: None,
                by: None,
            })?;
            print_json(&serde_json::to_value(result)?)
        }
        Cmd::Render { .. } => unreachable!("rendered offline"),
    }
}

fn create(
    client: &mut DaemonClient,
    agent: String,
    cwd: Option<String>,
    name: Option<String>,
    sandbox: Option<String>,
    first_prompt: &[String],
) -> anyhow::Result<()> {
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?.to_string_lossy().into_owned(),
    };
    let sandbox_level = sandbox
        .map(|level| match level.as_str() {
            "strict" => Ok(SandboxLevel::Strict),
            "relaxed" => Ok(SandboxLevel::Relaxed),
            "off" => Ok(SandboxLevel::Off),
            other => Err(anyhow::anyhow!("unknown sandbox level {other:?}")),
        })
        .transpose()?;
    let created = client.call::<methods::SessionCreate>(SessionCreateParams {
        op_id: OpId::generate(),
        agent,
        cwd: cwd.into(),
        name,
        parent_id: None,
        spawned_by: None,
        sandbox_level,
        sandbox: None,
        prompt: (!first_prompt.is_empty()).then(|| InitialPrompt {
            content: vec![ContentPart::text(first_prompt.join(" "))],
            origin: Default::default(),
        }),
    })?;
    print_json(&serde_json::to_value(&created.session)?)?;
    if let Some(prompt) = created.prompt {
        print_json(&serde_json::to_value(prompt)?)?;
    }
    Ok(())
}

fn prompt(
    client: &mut DaemonClient,
    session: SessionId,
    words: &[String],
) -> anyhow::Result<PromptResult> {
    Ok(client.call::<methods::SessionPrompt>(SessionPromptParams {
        op_id: OpId::generate(),
        id: session,
        content: vec![ContentPart::text(words.join(" "))],
        origin: Default::default(),
        if_busy: Default::default(),
    })?)
}

fn print_json(value: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn watch(client: &mut DaemonClient, session: SessionId, since: u64) -> anyhow::Result<()> {
    let subscription = client.call::<methods::SessionSubscribe>(SessionSubscribeParams {
        id: session,
        since_seq: Seq(since),
        raw_since: None,
        timeline: true,
    })?;
    let mut renderer = Renderer::new();
    let mut stdout = std::io::stdout().lock();
    loop {
        let notification = match client.next_notification() {
            Ok(n) => n,
            Err(client::CallError::Closed) => break,
            Err(e) => return Err(e.into()),
        };
        if notification.method != SessionEvents::NAME {
            continue;
        }
        let events: SessionEventsParams =
            serde_json::from_value(notification.params).context("malformed session.events")?;
        if events.subscription_id != subscription.subscription_id {
            continue;
        }
        for op in &events.ops {
            renderer.apply(op);
        }
        stdout.write_all(renderer.take().as_bytes())?;
        stdout.flush()?;
    }
    stdout.write_all(renderer.finish().as_bytes())?;
    Ok(())
}

fn render_file(file: Option<PathBuf>) -> anyhow::Result<()> {
    let input: Box<dyn BufRead> = match &file {
        Some(path) => Box::new(std::io::BufReader::new(
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?,
        )),
        None => Box::new(std::io::stdin().lock()),
    };
    let mut renderer = Renderer::new();
    let mut stdout = std::io::stdout().lock();
    for (n, line) in input.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        for op in ops_of_line(&line).with_context(|| format!("line {}", n + 1))? {
            renderer.apply(&op);
        }
        stdout.write_all(renderer.take().as_bytes())?;
    }
    stdout.write_all(renderer.finish().as_bytes())?;
    Ok(())
}

/// The ops in one input line: a `session.events` notification, its params, or one op.
pub fn ops_of_line(line: &str) -> anyhow::Result<Vec<TimelineOp>> {
    let mut value: Value = serde_json::from_str(line).context("not JSON")?;
    if value.get("method").and_then(Value::as_str) == Some(SessionEvents::NAME) {
        value = value["params"].take();
    }
    if value.get("ops").is_some() {
        let events: SessionEventsParams = serde_json::from_value(value)?;
        return Ok(events.ops);
    }
    if value.get("op").is_some() {
        return Ok(vec![serde_json::from_value(value)?]);
    }
    bail!("expected session.events, its params, or a timeline op")
}
