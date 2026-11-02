//! [`FakeAgent`]: the scripted agent behind the fake driver's stdin pipe.

use std::collections::{HashSet, VecDeque};

use controlroom_protocol::daemon::OptionValue;
use controlroom_protocol::session::{
    OptionControl, RequestKind, RequestOption, Response, RunOutcome, SessionOption,
};

use super::wire::{self, Command, Input, Output, WireNative, WireTag};

/// One step of a scripted turn.
#[derive(Debug, Clone, PartialEq)]
pub enum FakeStep {
    /// Stream agent text.
    Text(String),
    /// Run a tool. With a `gate`, the tool stays open until the gate is released. A tool right
    /// after a denied request fails with "Permission denied".
    Tool {
        /// The one-line title.
        title: String,
        /// Its output.
        output: String,
        /// Hold the tool open at this gate.
        gate: Option<String>,
    },
    /// Raise a request and wait for its answer. An option id starting with `allow` (or any
    /// answer to a question) lets the turn go on; anything else denies the next tool.
    Request {
        /// What kind.
        kind: RequestKind,
        /// The title.
        title: String,
    },
    /// Wait until the test releases this gate (or continue if it already did).
    Gate(String),
    /// Report context usage.
    Context {
        /// Tokens used.
        used: u64,
        /// The window size.
        size: u64,
    },
    /// Report a new session title.
    Title(String),
    /// End the run as failed with this error.
    Fail(String),
    /// Exit the process with this code, mid-run.
    Exit(i32),
}

/// A scripted turn: used for prompts containing `matches` (the first match wins).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FakeTurn {
    /// A substring of the prompt; `None` for the default turn.
    pub matches: Option<String>,
    /// What the agent does.
    pub steps: Vec<FakeStep>,
}

/// What a fake agent does, per prompt.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FakeScript {
    /// Turns tried in order.
    pub turns: Vec<FakeTurn>,
    /// The turn for prompts no other turn matches (empty: reply "ok").
    pub default_turn: Vec<FakeStep>,
    /// The agent's options.
    pub options: Vec<SessionOption>,
    /// After a new session the agent reports nothing until asked (like pi).
    pub quiet_switch: bool,
}

impl FakeScript {
    /// Adds a turn for prompts containing `matches`.
    pub fn turn(mut self, matches: &str, steps: impl IntoIterator<Item = FakeStep>) -> Self {
        self.turns.push(FakeTurn {
            matches: Some(matches.to_owned()),
            steps: steps.into_iter().collect(),
        });
        self
    }

    /// Sets the default turn.
    pub fn default_turn(mut self, steps: impl IntoIterator<Item = FakeStep>) -> Self {
        self.default_turn = steps.into_iter().collect();
        self
    }

    /// Sets the agent's options.
    pub fn options(mut self, options: Vec<SessionOption>) -> Self {
        self.options = options;
        self
    }

    /// Makes the agent report a new session only when asked (like pi).
    pub fn quiet_switch(mut self) -> Self {
        self.quiet_switch = true;
        self
    }

    fn steps_for(&self, prompt: &str) -> Vec<FakeStep> {
        let turn = self
            .turns
            .iter()
            .find(|t| t.matches.as_deref().is_some_and(|m| prompt.contains(m)));
        match turn {
            Some(turn) => turn.steps.clone(),
            None if self.default_turn.is_empty() => vec![FakeStep::Text("ok".into())],
            None => self.default_turn.clone(),
        }
    }
}

/// What the agent printed in reaction to an input or a gate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentOutput {
    /// Stdout lines, in order.
    pub lines: Vec<String>,
    /// The process exited with this code after the lines.
    pub exit: Option<i32>,
}

/// Internal steps: a script step, or the end of an open tool.
#[derive(Debug, Clone)]
enum Step {
    Script(FakeStep),
    ToolEnd { id: String, output: String },
}

/// What the run waits for.
#[derive(Debug, Clone, PartialEq)]
enum Wait {
    Nothing,
    Gate(String),
    Answer(String),
}

#[derive(Debug)]
struct Run {
    steps: VecDeque<Step>,
    wait: Wait,
    open_tool: Option<String>,
    denied: bool,
}

/// A scripted agent process. Pure: [`FakeAgent::input`] and [`FakeAgent::release`] return
/// what it prints.
#[derive(Debug)]
pub struct FakeAgent {
    script: FakeScript,
    native: WireNative,
    run: Option<Run>,
    released: HashSet<String>,
    counter: u64,
}

impl FakeAgent {
    /// Starts an agent with the fake driver's launch arguments (`--session-id <id>` or
    /// `--resume <id>`).
    pub fn launch(script: FakeScript, args: &[String]) -> Self {
        let id = args
            .windows(2)
            .find(|w| w[0] == "--session-id" || w[0] == "--resume")
            .map_or_else(|| "fake-session".to_owned(), |w| w[1].clone());
        FakeAgent {
            script,
            native: WireNative {
                path: Some(format!("/home/alice/.fake/{id}.jsonl")),
                id,
            },
            run: None,
            released: HashSet::new(),
            counter: 0,
        }
    }

    /// Handles one stdin line.
    pub fn input(&mut self, line: &str) -> AgentOutput {
        let mut out = AgentOutput::default();
        let Ok(Input { tag, command }) = serde_json::from_str::<Input>(line) else {
            return out;
        };
        match command {
            Command::Prompt { text } => {
                if self.run.is_some() {
                    return refuse(out, tag, "already processing");
                }
                self.run = Some(Run {
                    steps: self
                        .script
                        .steps_for(&text)
                        .into_iter()
                        .map(Step::Script)
                        .collect(),
                    wait: Wait::Nothing,
                    open_tool: None,
                    denied: false,
                });
                emit(
                    &mut out,
                    &Output::RunStart {
                        cause: Some(tag.clone()),
                    },
                );
                respond(&mut out, tag);
                self.advance(&mut out);
            }
            Command::Steer { text } => {
                if self.run.is_none() {
                    return refuse(out, tag, "no active run");
                }
                respond(&mut out, tag);
                emit(
                    &mut out,
                    &Output::Text {
                        text: format!("Steered: {text}"),
                    },
                );
            }
            Command::Cancel => {
                respond(&mut out, tag);
                self.cancel(&mut out);
            }
            Command::Answer { request, response } => {
                let Some(run) = self
                    .run
                    .as_mut()
                    .filter(|r| r.wait == Wait::Answer(request))
                else {
                    return refuse(out, tag, "no such request");
                };
                run.wait = Wait::Nothing;
                run.denied = match &response {
                    Response::Choice { option_id, .. } => !option_id.starts_with("allow"),
                    Response::Answers { .. } => false,
                    Response::Dismiss | Response::Unknown => true,
                };
                respond(&mut out, tag);
                self.advance(&mut out);
            }
            Command::SetOption { id, value } => {
                let Some(option) = self.script.options.iter_mut().find(|o| o.id == id) else {
                    return refuse(out, tag, "unknown option");
                };
                match (&mut option.control, value) {
                    (OptionControl::Select { value, .. }, OptionValue::Choice(new)) => {
                        *value = new;
                    }
                    (OptionControl::Bool { value }, OptionValue::Bool(new)) => *value = new,
                    _ => return refuse(out, tag, "invalid value"),
                }
                respond(&mut out, tag);
                self.state(&mut out);
            }
            Command::NewSession { .. } => {
                self.counter += 1;
                self.native = WireNative {
                    id: format!("{}-{}", self.native.id, self.counter),
                    path: None,
                };
                respond(&mut out, tag);
                if !self.script.quiet_switch {
                    self.state(&mut out);
                }
            }
            Command::State => {
                respond(&mut out, tag);
                self.state(&mut out);
            }
        }
        out
    }

    /// Releases a gate: the run continues if it waits there; otherwise the gate is passed
    /// when the run reaches it.
    pub fn release(&mut self, gate: &str) -> AgentOutput {
        let mut out = AgentOutput::default();
        match self.run.as_mut() {
            Some(run) if run.wait == Wait::Gate(gate.to_owned()) => {
                run.wait = Wait::Nothing;
                self.advance(&mut out);
            }
            _ => {
                self.released.insert(gate.to_owned());
            }
        }
        out
    }

    fn state(&self, out: &mut AgentOutput) {
        emit(
            out,
            &Output::State {
                native: self.native.clone(),
                options: self.script.options.clone(),
            },
        );
    }

    fn cancel(&mut self, out: &mut AgentOutput) {
        let Some(run) = self.run.take() else { return };
        if let Wait::Answer(id) = run.wait {
            emit(out, &Output::RequestClosed { id });
        }
        if let Some(id) = run.open_tool {
            let output = "cancelled".into();
            emit(
                out,
                &Output::ToolEnd {
                    id,
                    ok: false,
                    output,
                },
            );
        }
        let end = Output::RunEnd {
            outcome: RunOutcome::Cancelled,
            error: None,
        };
        emit(out, &end);
    }

    /// Runs steps until the run waits or ends.
    fn advance(&mut self, out: &mut AgentOutput) {
        loop {
            let Some(run) = self.run.as_mut() else { return };
            if run.wait != Wait::Nothing {
                return;
            }
            let Some(step) = run.steps.pop_front() else {
                self.run = None;
                let end = Output::RunEnd {
                    outcome: RunOutcome::Completed,
                    error: None,
                };
                emit(out, &end);
                return;
            };
            self.counter += 1;
            let n = self.counter;
            match step {
                Step::Script(FakeStep::Text(text)) => emit(out, &Output::Text { text }),
                Step::Script(FakeStep::Tool {
                    title,
                    output,
                    gate,
                }) => {
                    let id = format!("tool{n}");
                    emit(
                        out,
                        &Output::ToolStart {
                            id: id.clone(),
                            name: "bash".into(),
                            title,
                        },
                    );
                    if std::mem::take(&mut run.denied) {
                        let output = "Permission denied".into();
                        emit(
                            out,
                            &Output::ToolEnd {
                                id,
                                ok: false,
                                output,
                            },
                        );
                        continue;
                    }
                    run.open_tool = Some(id.clone());
                    run.steps.push_front(Step::ToolEnd { id, output });
                    if let Some(gate) = gate {
                        run.steps.push_front(Step::Script(FakeStep::Gate(gate)));
                    }
                }
                Step::ToolEnd { id, output } => {
                    run.open_tool = None;
                    emit(
                        out,
                        &Output::ToolEnd {
                            id,
                            ok: true,
                            output,
                        },
                    );
                }
                Step::Script(FakeStep::Request { kind, title }) => {
                    let id = format!("req{n}");
                    run.wait = Wait::Answer(id.clone());
                    emit(
                        out,
                        &Output::Request {
                            id,
                            kind,
                            title,
                            options: request_options(),
                            questions: Vec::new(),
                        },
                    );
                }
                Step::Script(FakeStep::Gate(gate)) => {
                    if !self.released.remove(&gate) {
                        run.wait = Wait::Gate(gate);
                    }
                }
                Step::Script(FakeStep::Context { used, size }) => emit(
                    out,
                    &Output::Context {
                        used,
                        size: Some(size),
                        cost: None,
                    },
                ),
                Step::Script(FakeStep::Title(title)) => emit(out, &Output::Title { title }),
                Step::Script(FakeStep::Fail(error)) => {
                    self.run = None;
                    let end = Output::RunEnd {
                        outcome: RunOutcome::Failed,
                        error: Some(error),
                    };
                    emit(out, &end);
                    return;
                }
                Step::Script(FakeStep::Exit(code)) => {
                    self.run = None;
                    out.exit = Some(code);
                    return;
                }
            }
        }
    }
}

fn request_options() -> Vec<RequestOption> {
    use controlroom_protocol::session::OptionStyle;
    let option = |id: &str, label: &str, style| RequestOption {
        id: id.into(),
        label: label.into(),
        style,
    };
    vec![
        option("allow-once", "Allow once", OptionStyle::Allow),
        option("reject", "Reject", OptionStyle::Deny),
    ]
}

fn emit(out: &mut AgentOutput, output: &Output) {
    out.lines.push(wire::line(output));
}

fn respond(out: &mut AgentOutput, tag: WireTag) {
    emit(
        out,
        &Output::Response {
            tag,
            ok: true,
            error: None,
        },
    );
}

fn refuse(mut out: AgentOutput, tag: WireTag, error: &str) -> AgentOutput {
    let error = Some(error.to_owned());
    emit(
        &mut out,
        &Output::Response {
            tag,
            ok: false,
            error,
        },
    );
    out
}
