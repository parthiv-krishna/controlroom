//! [`FakeDriver`]: encodes intents into [`wire`] lines and folds the transcript.

use std::collections::HashMap;

use controlroom_protocol::daemon::OptionValue;
use controlroom_protocol::ids::{ItemId, RequestId};
use controlroom_protocol::session::{
    ContentPart, ContextUsage, DriverKind, OptionControl, Request, RequestAnswer, RequestState,
    Response, RunOutcome,
};
use controlroom_protocol::timeline::{
    Actor, ItemBody, MessageRole, RunState, RunTrigger, TimelineItem, TimelineOp, ToolKind,
    ToolOutput, ToolStatus,
};
use controlroom_protocol::{Seq, UnixMillis};
use controlroom_transcript::{Body, Record};

use super::wire::{self, Command, Input, Output, WireTag};
use crate::{
    AgentView, Capabilities, Driver, DriverEvent, EncodeError, FoldOut, Intent, LineTag, StartMode,
};

/// The deterministic in-process driver for runtime tests. Item ids are derived from the seq of
/// the record that created the item, so a re-fold yields the same ids.
#[derive(Debug, Default)]
pub struct FakeDriver {
    view: AgentView,
    /// The active run's item.
    run: Option<TimelineItem>,
    /// The agent message being streamed.
    message: Option<ItemId>,
    /// Open tool calls by the agent's call id.
    tools: HashMap<String, TimelineItem>,
    /// Requests by the agent's request id (open and answered ones of this process).
    requests: HashMap<String, TimelineItem>,
}

impl FakeDriver {
    /// A driver with these capabilities.
    pub fn new(capabilities: Capabilities) -> Self {
        FakeDriver {
            view: AgentView {
                capabilities,
                ..AgentView::default()
            },
            ..FakeDriver::default()
        }
    }

    /// Drops everything that belongs to the agent process (a new one starts, or it exited).
    fn reset_process(&mut self, out: &mut FoldOut, rec: &Record) {
        self.end_message(out, rec);
        if let Some(mut run) = self.run.take() {
            set_run(&mut run, RunState::Settled, Some(RunOutcome::Unknown), None);
            out.ops.push(upsert(run, rec));
        }
        for (_, mut tool) in self.tools.drain() {
            if let ItemBody::ToolCall { status, .. } = &mut tool.body {
                *status = ToolStatus::Cancelled;
            }
            out.ops.push(upsert(tool, rec));
        }
        self.close_requests(out, rec);
        self.requests.clear();
        let capabilities = self.view.capabilities;
        let native = self.view.native.take();
        let options = std::mem::take(&mut self.view.options);
        let last_outcome = self.view.last_outcome;
        self.view = AgentView {
            capabilities,
            native,
            options,
            last_outcome,
            last_seq: self.view.last_seq,
            last_ts: self.view.last_ts,
            ..AgentView::default()
        };
    }

    fn close_requests(&mut self, out: &mut FoldOut, rec: &Record) {
        for item in self.requests.values_mut() {
            if let ItemBody::Request { request } = &mut item.body
                && request.state == RequestState::Pending
            {
                request.state = RequestState::Withdrawn;
                out.events.push(DriverEvent::RequestClosed {
                    id: request.id.clone(),
                });
                out.ops.push(upsert(item.clone(), rec));
            }
        }
        self.view.open_requests.clear();
    }

    fn end_message(&mut self, out: &mut FoldOut, rec: &Record) {
        if let Some(id) = self.message.take() {
            out.ops.push(TimelineOp::End {
                id,
                seq_from: Seq(rec.seq),
                seq_to: Seq(rec.seq),
            });
        }
    }

    fn fold_in(&mut self, raw: &str, rec: &Record, out: &mut FoldOut) {
        let Ok(input) = serde_json::from_str::<Input>(raw) else {
            return;
        };
        if let Some(tag) = input.tag.to_tag() {
            self.view.unanswered.push(tag.clone());
            out.events.push(DriverEvent::Written { tag });
        }
        match input.command {
            Command::Prompt { text } | Command::Steer { text } => {
                self.end_message(out, rec);
                let item = new_item(
                    rec,
                    "u",
                    false,
                    ItemBody::Message {
                        role: MessageRole::User,
                        content: vec![ContentPart::text(text)],
                    },
                );
                out.ops.push(upsert(item, rec));
            }
            Command::Answer { request, response } => {
                let Some(item) = self.requests.get_mut(&request) else {
                    return;
                };
                if let ItemBody::Request { request: req } = &mut item.body {
                    req.state = RequestState::Answered;
                    req.answer = Some(RequestAnswer {
                        label: answer_label(req, &response),
                        response,
                        by: None,
                        at: ts(rec),
                    });
                    let id = req.id.clone();
                    self.view.open_requests.retain(|open| *open != id);
                }
                out.ops.push(upsert(item.clone(), rec));
            }
            Command::Cancel | Command::SetOption { .. } | Command::NewSession { .. } => {}
            Command::State => {}
        }
    }

    fn fold_out(&mut self, raw: &str, rec: &Record, out: &mut FoldOut) {
        let Ok(output) = serde_json::from_str::<Output>(raw) else {
            return;
        };
        if !self.view.ready {
            self.view.ready = true;
            out.events.push(DriverEvent::Ready);
        }
        match output {
            Output::Response { tag, ok, error } => {
                let Some(tag) = tag.to_tag() else { return };
                self.view.unanswered.retain(|t| *t != tag);
                out.events.push(if ok {
                    DriverEvent::Accepted { tag }
                } else {
                    DriverEvent::Rejected {
                        tag,
                        error: error.unwrap_or_default(),
                    }
                });
            }
            Output::State { native, options } => {
                let native: crate::NativeRef = native.into();
                if self.view.native.as_ref() != Some(&native) {
                    self.view.native = Some(native.clone());
                    out.events.push(DriverEvent::NativeSession(native));
                }
                self.view.options.clone_from(&options);
                out.events.push(DriverEvent::Options(options));
            }
            Output::RunStart { cause } => {
                self.end_message(out, rec);
                let cause = cause.as_ref().and_then(WireTag::to_tag);
                let run = new_item(
                    rec,
                    "r",
                    false,
                    ItemBody::Run {
                        trigger: trigger(cause.as_ref()),
                        state: RunState::Running,
                        outcome: None,
                        error: None,
                    },
                );
                out.ops.push(upsert(run.clone(), rec));
                self.run = Some(run);
                self.view.run_active = true;
                out.events.push(DriverEvent::RunStarted { cause });
            }
            Output::RunEnd { outcome, error } => {
                self.end_message(out, rec);
                if let Some(mut run) = self.run.take() {
                    set_run(&mut run, RunState::Settled, Some(outcome), error.clone());
                    out.ops.push(upsert(run, rec));
                }
                self.close_requests(out, rec);
                self.view.run_active = false;
                self.view.open_tools = 0;
                self.tools.clear();
                self.view.last_outcome = Some(outcome);
                out.events.push(DriverEvent::RunSettled { outcome, error });
            }
            Output::Text { text } => match &self.message {
                Some(id) => out.ops.push(TimelineOp::AppendText {
                    id: id.clone(),
                    text,
                    seq_from: Seq(rec.seq),
                    seq_to: Seq(rec.seq),
                }),
                None => {
                    let item = new_item(
                        rec,
                        "m",
                        true,
                        ItemBody::Message {
                            role: MessageRole::Agent,
                            content: vec![ContentPart::text(text)],
                        },
                    );
                    self.message = Some(item.id.clone());
                    out.ops.push(upsert(item, rec));
                }
            },
            Output::ToolStart { id, name, title } => {
                self.end_message(out, rec);
                let item = new_item(
                    rec,
                    "t",
                    false,
                    ItemBody::ToolCall {
                        tool_call_id: id.clone(),
                        parent_tool_call_id: None,
                        name,
                        title,
                        tool_kind: ToolKind::Execute,
                        input: None,
                        status: ToolStatus::Running,
                        output: None,
                        exit: None,
                        duration_ms: None,
                    },
                );
                out.ops.push(upsert(item.clone(), rec));
                self.tools.insert(id, item);
                self.view.open_tools = self.tools.len();
            }
            Output::ToolEnd { id, ok, output } => {
                let Some(mut item) = self.tools.remove(&id) else {
                    return;
                };
                if let ItemBody::ToolCall {
                    status, output: o, ..
                } = &mut item.body
                {
                    *status = if ok {
                        ToolStatus::Completed
                    } else {
                        ToolStatus::Failed
                    };
                    *o = Some(ToolOutput::Text { text: output });
                }
                out.ops.push(upsert(item, rec));
                self.view.open_tools = self.tools.len();
            }
            Output::Request {
                id,
                kind,
                title,
                options,
                questions,
            } => {
                self.end_message(out, rec);
                let request = Request {
                    id: RequestId::new(format!("q{}", rec.seq)),
                    kind,
                    blocking: kind != controlroom_protocol::session::RequestKind::Question,
                    state: RequestState::Pending,
                    title,
                    detail: None,
                    body: None,
                    options,
                    questions,
                    answer: None,
                    origin_child: None,
                    created_at: ts(rec),
                };
                self.view.open_requests.push(request.id.clone());
                let item = new_item(
                    rec,
                    "q",
                    false,
                    ItemBody::Request {
                        request: request.clone(),
                    },
                );
                out.ops.push(upsert(item.clone(), rec));
                self.requests.insert(id, item);
                out.events
                    .push(DriverEvent::RequestOpened(Box::new(request)));
            }
            Output::RequestClosed { id } => {
                let Some(item) = self.requests.get_mut(&id) else {
                    return;
                };
                if let ItemBody::Request { request } = &mut item.body
                    && request.state == RequestState::Pending
                {
                    request.state = RequestState::Withdrawn;
                    let id = request.id.clone();
                    self.view.open_requests.retain(|open| *open != id);
                    out.events.push(DriverEvent::RequestClosed { id });
                    out.ops.push(upsert(item.clone(), rec));
                }
            }
            Output::Context { used, size, cost } => {
                out.events
                    .push(DriverEvent::Context(ContextUsage { used, size, cost }));
            }
            Output::Title { title } => out.events.push(DriverEvent::Title(title)),
        }
    }

    fn agent_request_id(&self, request: &RequestId) -> Option<&str> {
        self.requests
            .iter()
            .find_map(|(agent_id, item)| match &item.body {
                ItemBody::Request { request: r } if r.id == *request => Some(agent_id.as_str()),
                _ => None,
            })
    }

    fn check_option(&self, id: &str, value: &OptionValue) -> Result<(), EncodeError> {
        let option = self
            .view
            .options
            .iter()
            .find(|o| o.id == id)
            .ok_or_else(|| EncodeError::Invalid(format!("unknown option {id:?}")))?;
        let valid = match (&option.control, value) {
            (OptionControl::Select { choices, .. }, OptionValue::Choice(v)) => {
                choices.iter().any(|c| c.value == *v)
            }
            (OptionControl::Bool { .. }, OptionValue::Bool(_)) => true,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(EncodeError::Invalid(format!("invalid value for {id:?}")))
        }
    }
}

impl Driver for FakeDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Fake
    }

    fn fold(&mut self, rec: &Record) -> FoldOut {
        let mut out = FoldOut::default();
        match &rec.body {
            Body::Meta(_) => {
                self.reset_process(&mut out, rec);
                // A new process reports its native session again.
                self.view.native = None;
            }
            Body::In(line) if line.truncated == 0 => self.fold_in(&line.raw, rec, &mut out),
            Body::Out(line) if line.truncated == 0 => self.fold_out(&line.raw, rec, &mut out),
            Body::Exit(exit) => {
                let during_run = self.view.run_active;
                self.reset_process(&mut out, rec);
                out.events.push(DriverEvent::Exited {
                    code: exit.code,
                    signal: exit.signal,
                    during_run,
                });
            }
            Body::In(_) | Body::Out(_) | Body::Stderr(_) | Body::Unknown(_) => {}
        }
        self.view.last_seq = Some(Seq(rec.seq));
        self.view.last_ts = Some(ts(rec));
        out
    }

    fn view(&self) -> &AgentView {
        &self.view
    }

    fn encode(&mut self, intent: &Intent, tag: &LineTag) -> Result<Vec<String>, EncodeError> {
        let caps = self.view.capabilities;
        let command = match intent {
            Intent::Prompt { content } => {
                if self.view.run_active {
                    return Err(EncodeError::InvalidState("a run is active".into()));
                }
                Command::Prompt {
                    text: plain_text(content),
                }
            }
            Intent::Steer { content } if caps.steer => Command::Steer {
                text: plain_text(content),
            },
            Intent::Inject { content, .. } if !self.view.run_active => Command::Prompt {
                text: plain_text(content),
            },
            Intent::Inject { content, .. } if caps.steer => Command::Steer {
                text: plain_text(content),
            },
            Intent::Steer { .. } | Intent::FollowUp { .. } | Intent::Inject { .. } => {
                return Err(EncodeError::Unsupported(
                    "the fake agent can't do that".into(),
                ));
            }
            Intent::Cancel => Command::Cancel,
            Intent::Answer { request, response } => Command::Answer {
                request: self
                    .agent_request_id(request)
                    .ok_or_else(|| EncodeError::Invalid(format!("unknown request {request}")))?
                    .to_owned(),
                response: response.clone(),
            },
            Intent::SetOption { id, value } => {
                self.check_option(id, value)?;
                Command::SetOption {
                    id: id.clone(),
                    value: value.clone(),
                }
            }
            Intent::NewSession { parent } if caps.new_session => Command::NewSession {
                parent: parent.clone(),
            },
            Intent::NewSession { .. } => {
                return Err(EncodeError::Unsupported("no new_session".into()));
            }
            Intent::Refresh => Command::State,
        };
        Ok(vec![wire::line(&Input {
            tag: WireTag::from(tag),
            command,
        })])
    }

    fn launch_args(&self, mode: &StartMode) -> Vec<String> {
        match mode {
            StartMode::New { native_id } => vec!["--session-id".into(), native_id.clone()],
            StartMode::Resume { native } => vec!["--resume".into(), native.id.clone()],
        }
    }
}

fn ts(rec: &Record) -> UnixMillis {
    UnixMillis(i64::try_from(rec.ts).unwrap_or(i64::MAX))
}

fn new_item(rec: &Record, prefix: &str, streaming: bool, body: ItemBody) -> TimelineItem {
    TimelineItem {
        id: ItemId::new(format!("{prefix}{}", rec.seq)),
        seq_from: Seq(rec.seq),
        seq_to: Seq(rec.seq),
        actor: Actor::Root,
        ts: ts(rec),
        streaming,
        body,
    }
}

fn upsert(mut item: TimelineItem, rec: &Record) -> TimelineOp {
    item.seq_to = Seq(rec.seq);
    TimelineOp::Upsert {
        item: Box::new(item),
    }
}

fn set_run(run: &mut TimelineItem, new: RunState, out: Option<RunOutcome>, err: Option<String>) {
    if let ItemBody::Run {
        state,
        outcome,
        error,
        ..
    } = &mut run.body
    {
        *state = new;
        *outcome = out;
        *error = err;
    }
}

/// The run trigger a tag implies: user commands, queue items, daemon-internal lines.
fn trigger(cause: Option<&LineTag>) -> RunTrigger {
    match cause {
        Some(LineTag::Op { .. }) => RunTrigger::User { device: None },
        Some(LineTag::Queue { .. }) => RunTrigger::Queue { device: None },
        Some(LineTag::Internal { .. }) | None => RunTrigger::Unknown,
    }
}

fn plain_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn answer_label(request: &Request, response: &Response) -> String {
    match response {
        Response::Choice { option_id, .. } => request
            .options
            .iter()
            .find(|o| o.id == *option_id)
            .map_or_else(|| option_id.clone(), |o| o.label.clone()),
        Response::Answers { answers } => answers
            .iter()
            .flat_map(|a| a.selected.iter().cloned().chain(a.text.clone()))
            .collect::<Vec<_>>()
            .join(", "),
        Response::Dismiss => "Dismissed".into(),
        Response::Unknown => "Answered".into(),
    }
}
