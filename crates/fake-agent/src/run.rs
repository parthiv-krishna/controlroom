//! One agent run, from `agent_start` to `agent_settled`, following pi's
//! loop: turns of (queued user messages, one model reply, tool calls),
//! steering at turn starts, follow-ups when the agent would stop, and
//! agent-level retries of transient errors.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::agent::{Agent, QueueMode, Waited, abortable};
use crate::ids::now_ms;
use crate::scenario::{Reply, Step, TextStep, ToolCallStep, ToolProgress};
use crate::wire;

/// pi's error text for a model call made after an abort.
const ABORTED_CALL: &str = "This operation was aborted";
/// pi's error text for a stream cut by an abort.
const ABORTED_STREAM: &str = "Request was aborted";

/// Error messages pi treats as transient (a subset of pi-ai's pattern).
const RETRYABLE: &[&str] = &[
    "overloaded",
    "rate limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "service unavailable",
    "server error",
    "internal error",
    "connection",
    "fetch failed",
];

fn is_retryable(message: &str) -> bool {
    let lower = message.to_lowercase();
    RETRYABLE.iter().any(|pattern| lower.contains(pattern))
}

/// How a sequence of turns ended.
struct TurnsEnd {
    aborted: bool,
    /// The failed assistant message's error and entry id.
    error: Option<(String, String)>,
    messages: Vec<Value>,
}

/// What one assistant response produced.
enum Response {
    Finished {
        tools: Vec<ToolCallStep>,
    },
    Aborted,
    Failed {
        message: String,
        entry: String,
        retryable: bool,
    },
}

/// Run a prompt to settlement. `done` is set when the run has settled.
pub async fn run(
    agent: Arc<Agent>,
    prompt: String,
    mut abort: watch::Receiver<bool>,
    done: watch::Sender<bool>,
) {
    let mut pending = vec![prompt];
    let mut attempt = 0u32;
    loop {
        agent.emit(&json!({"type": "agent_start"}));
        let end = turns(&agent, &mut pending, &mut attempt, &mut abort).await;
        let max = agent.scenario.max_retries;
        let retry = match &end.error {
            Some((message, entry)) if !end.aborted && attempt < max && agent.state().auto_retry => {
                is_retryable(message).then(|| (message.clone(), entry.clone()))
            }
            _ => None,
        };
        agent.emit(&json!({
            "type": "agent_end", "messages": end.messages, "willRetry": retry.is_some()
        }));
        let Some((message, target)) = retry else {
            if let (Some((message, _)), true) = (&end.error, attempt > 0) {
                agent.emit(&json!({
                    "type": "auto_retry_end", "success": false, "attempt": attempt,
                    "finalError": message
                }));
            }
            settle(&agent, end.aborted, &done);
            return;
        };
        attempt += 1;
        let delay = agent.scenario.retry_base_delay_ms << (attempt - 1);
        agent.emit(&json!({
            "type": "auto_retry_start", "attempt": attempt, "maxAttempts": max,
            "delayMs": delay, "errorMessage": message
        }));
        let edit =
            agent.persist(json!({"type": "context_edit", "targetId": target, "replacement": null}));
        agent.emit(&json!({"type": "entry_appended", "entry": edit}));
        let sleep = tokio::time::sleep(Duration::from_millis(delay));
        if let Waited::Aborted = abortable(&mut abort, sleep).await {
            agent.emit(&json!({
                "type": "auto_retry_end", "success": false, "attempt": attempt,
                "finalError": "Retry cancelled"
            }));
            settle(&agent, true, &done);
            return;
        }
    }
}

fn settle(agent: &Agent, aborted: bool, done: &watch::Sender<bool>) {
    agent.emit(&json!({"type": "agent_settled", "aborted": aborted}));
    agent.state().run = None;
    done.send_replace(true);
}

/// Pop the next queued message per the queue's mode.
fn pop(queue: &mut std::collections::VecDeque<String>, mode: QueueMode) -> Vec<String> {
    match mode {
        QueueMode::All => queue.drain(..).collect(),
        QueueMode::OneAtATime => queue.pop_front().into_iter().collect(),
    }
}

/// Turns until the agent stops, fails or is aborted.
async fn turns(
    agent: &Arc<Agent>,
    pending: &mut Vec<String>,
    attempt: &mut u32,
    abort: &mut watch::Receiver<bool>,
) -> TurnsEnd {
    let mut messages = Vec::new();
    let mut follow_up_turn = false;
    loop {
        agent.emit(&json!({"type": "turn_start"}));
        let mut push = |agent: &Agent, message: Value| {
            agent.whole_message(&message);
            messages.push(message);
        };
        if !agent.state().session.has_system() {
            push(agent, wire::system_message(&agent.cwd));
        }
        for text in pending.drain(..) {
            push(agent, wire::user_message(&text));
        }
        let queued = |agent: &Agent, follow: bool| {
            let mut state = agent.state();
            let taken = if follow {
                let mode = state.follow_up_mode;
                pop(&mut state.follow_up, mode)
            } else {
                let mode = state.steering_mode;
                pop(&mut state.steering, mode)
            };
            drop(state);
            if !taken.is_empty() {
                agent.emit(&agent.queue_update());
            }
            taken
        };
        if follow_up_turn {
            for text in queued(agent, true) {
                push(agent, wire::user_message(&text));
            }
        }
        for text in queued(agent, false) {
            push(agent, wire::user_message(&text));
        }

        let reply = agent.next_reply();
        match respond(agent, reply, abort, &mut messages).await {
            Response::Aborted => {
                end_turn(agent, &mut messages, &[]);
                return TurnsEnd {
                    aborted: true,
                    error: None,
                    messages,
                };
            }
            Response::Failed {
                message,
                entry,
                retryable,
            } => {
                end_turn(agent, &mut messages, &[]);
                let aborted = !retryable && *abort.borrow();
                return TurnsEnd {
                    aborted,
                    error: (!aborted).then_some((message, entry)),
                    messages,
                };
            }
            Response::Finished { tools } => {
                if *attempt > 0 {
                    agent.emit(
                        &json!({"type": "auto_retry_end", "success": true, "attempt": *attempt}),
                    );
                    *attempt = 0;
                }
                if !tools.is_empty() {
                    let results = run_tools(agent, &tools, abort).await;
                    for result in &results {
                        agent.whole_message(result);
                    }
                    end_turn(agent, &mut messages, &results);
                    messages.extend(results);
                    continue;
                }
                end_turn(agent, &mut messages, &[]);
                let state = agent.state();
                if !state.steering.is_empty() {
                    follow_up_turn = false;
                } else if !state.follow_up.is_empty() {
                    follow_up_turn = true;
                } else {
                    return TurnsEnd {
                        aborted: false,
                        error: None,
                        messages,
                    };
                }
            }
        }
    }
}

/// `turn_end` with the turn's assistant message (the last one so far).
fn end_turn(agent: &Agent, messages: &mut [Value], results: &[Value]) {
    let message = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .cloned()
        .unwrap_or(Value::Null);
    agent.emit(&json!({"type": "turn_end", "message": message, "toolResults": results}));
}

/// A content block being streamed.
enum Block {
    Text(String),
    Thinking(String),
    Tool(Box<ToolCallStep>),
}

impl Block {
    fn content(&self) -> Value {
        match self {
            Self::Text(text) => json!({"type": "text", "text": text}),
            Self::Thinking(text) => json!({"type": "thinking", "thinking": text}),
            Self::Tool(tool) => json!({
                "type": "toolCall", "id": tool.id, "name": tool.name, "arguments": tool.args
            }),
        }
    }

    fn end_event(&self, index: usize) -> Value {
        let content = self.content();
        match self {
            Self::Text(text) => json!({"type": "text_end", "contentIndex": index, "content": text}),
            Self::Thinking(text) => {
                json!({"type": "thinking_end", "contentIndex": index, "content": text})
            }
            Self::Tool(_) => {
                json!({"type": "toolcall_end", "contentIndex": index, "toolCall": content})
            }
        }
    }
}

/// Stream one model reply as an assistant message, appended to `messages`.
async fn respond(
    agent: &Arc<Agent>,
    reply: Reply,
    abort: &mut watch::Receiver<bool>,
    messages: &mut Vec<Value>,
) -> Response {
    let model = agent.model();
    let timestamp = now_ms();
    let zero = wire::usage(None);
    let failed = |agent: &Agent, messages: &mut Vec<Value>, error: &str, retryable: bool| {
        let message = wire::assistant_message(
            &model,
            vec![],
            zero.clone(),
            "error",
            Some(error),
            timestamp,
        );
        let entry = agent.whole_message(&message);
        messages.push(message);
        Response::Failed {
            message: error.to_owned(),
            entry,
            retryable,
        }
    };
    if *abort.borrow() {
        return failed(agent, messages, ABORTED_CALL, false);
    }
    if let Some(error) = &reply.error {
        return failed(agent, messages, error, true);
    }

    let pending = wire::assistant_message(&model, vec![], zero.clone(), "pending", None, timestamp);
    agent.emit(&wire::message_event("message_start", &pending));
    let mut blocks: Vec<Block> = Vec::new();
    let mut aborted = false;
    for step in &reply.content {
        if *abort.borrow() {
            aborted = true;
            break;
        }
        match step {
            Step::Text(text) => stream_text(agent, &mut blocks, text, false, &zero),
            Step::Thinking(text) => stream_text(agent, &mut blocks, text, true, &zero),
            Step::ToolCall(tool) => {
                let index = blocks.len();
                let event = |delta: String| json!({"type": "toolcall_delta", "contentIndex": index, "delta": delta});
                agent.emit(&wire::message_update(
                    json!({"type": "toolcall_start", "contentIndex": index, "id": tool.id,
                           "toolName": tool.name}),
                    &zero,
                ));
                agent.emit(&wire::message_update(event(String::new()), &zero));
                agent.emit(&wire::message_update(event(tool.args.to_string()), &zero));
                blocks.push(Block::Tool(Box::new(tool.clone())));
            }
            side => {
                if let Waited::Aborted = agent.side_step(side, abort).await {
                    aborted = true;
                    break;
                }
            }
        }
    }
    for (index, block) in blocks.iter().enumerate() {
        agent.emit(&wire::message_update(block.end_event(index), &zero));
    }
    let tools: Vec<ToolCallStep> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Tool(tool) => Some((**tool).clone()),
            _ => None,
        })
        .collect();
    let (stop, error, usage) = if aborted {
        ("aborted", Some(ABORTED_STREAM), zero.clone())
    } else if tools.is_empty() {
        (
            reply.stop.as_str(),
            None,
            wire::usage(Some(reply.usage.unwrap_or_default())),
        )
    } else {
        (
            "toolUse",
            None,
            wire::usage(Some(reply.usage.unwrap_or_default())),
        )
    };
    let content = blocks.iter().map(Block::content).collect();
    let message = wire::assistant_message(&model, content, usage, stop, error, timestamp);
    agent.emit(&wire::message_event("message_end", &message));
    agent.persist_message(&message);
    messages.push(message);
    if aborted {
        Response::Aborted
    } else {
        Response::Finished { tools }
    }
}

/// Append `step` to the open text/thinking block, or open a new one.
fn stream_text(
    agent: &Agent,
    blocks: &mut Vec<Block>,
    step: &TextStep,
    thinking: bool,
    usage: &Value,
) {
    let (start, delta) = if thinking {
        ("thinking_start", "thinking_delta")
    } else {
        ("text_start", "text_delta")
    };
    let continues = matches!(
        (blocks.last(), thinking),
        (Some(Block::Thinking(_)), true) | (Some(Block::Text(_)), false)
    );
    if !continues {
        let index = blocks.len();
        agent.emit(&wire::message_update(
            json!({"type": start, "contentIndex": index}),
            usage,
        ));
        blocks.push(if thinking {
            Block::Thinking(String::new())
        } else {
            Block::Text(String::new())
        });
    }
    let index = blocks.len() - 1;
    let chars: Vec<char> = step.text.chars().collect();
    for chunk in chars.chunks(step.chunk.max(1)) {
        let chunk: String = chunk.iter().collect();
        agent.emit(&wire::message_update(
            json!({"type": delta, "contentIndex": index, "delta": chunk}),
            usage,
        ));
        if let Some(Block::Text(text) | Block::Thinking(text)) = blocks.last_mut() {
            text.push_str(&chunk);
        }
    }
}

/// How a tool call ended.
struct ToolOutcome {
    text: String,
    is_error: bool,
    /// How long `execute()` ran; `None` when the call was refused before it ran.
    duration_ms: Option<u64>,
}

impl ToolOutcome {
    fn refused(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            is_error: true,
            duration_ms: None,
        }
    }
}

/// Execute tool calls in order; returns their tool result messages.
async fn run_tools(
    agent: &Arc<Agent>,
    tools: &[ToolCallStep],
    abort: &mut watch::Receiver<bool>,
) -> Vec<Value> {
    let mut results = Vec::new();
    for tool in tools {
        agent.emit(&json!({
            "type": "tool_execution_start", "toolCallId": tool.id, "toolName": tool.name,
            "args": tool.args
        }));
        let outcome = execute(agent, tool, abort).await;
        let mut end = json!({
            "type": "tool_execution_end", "toolCallId": tool.id, "toolName": tool.name,
            "result": wire::tool_result(&outcome.text), "isError": outcome.is_error
        });
        let mut message =
            wire::tool_result_message(&tool.id, &tool.name, &outcome.text, outcome.is_error);
        if let Some(duration) = outcome.duration_ms {
            end["durationMs"] = duration.into();
            message["durationMs"] = duration.into();
        }
        agent.emit(&end);
        results.push(message);
    }
    results
}

/// A tool's scripted execution: permission (pi's `tool_call` hook, before
/// the tool runs), then nested calls and progress.
async fn execute(
    agent: &Arc<Agent>,
    tool: &ToolCallStep,
    abort: &mut watch::Receiver<bool>,
) -> ToolOutcome {
    if let Some(permission) = &tool.permission {
        let dialog = crate::scenario::DialogStep {
            method: crate::scenario::DialogMethod::Select,
            title: permission.title.clone(),
            options: permission.options.clone(),
            message: None,
            placeholder: None,
            prefill: None,
            timeout_ms: None,
        };
        let answer = agent.dialog(&dialog).await;
        if *abort.borrow() {
            return ToolOutcome::refused("Operation aborted");
        }
        let chosen = answer.as_ref().and_then(|a| a["value"].as_str());
        if chosen != Some(permission.allow.as_str()) {
            return ToolOutcome::refused("Blocked by user");
        }
    }
    let started = Instant::now();
    let elapsed = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    for (n, nested) in tool.nested.iter().enumerate() {
        let id = format!("{}/{}", tool.id, n + 1);
        agent.emit(&json!({
            "type": "tool_execution_start", "toolCallId": id, "toolName": nested.name,
            "args": nested.args, "parentToolCallId": tool.id
        }));
        agent.emit(&json!({
            "type": "tool_execution_end", "toolCallId": id, "toolName": nested.name,
            "result": wire::tool_result(&nested.result), "isError": false, "durationMs": 0,
            "parentToolCallId": tool.id
        }));
    }
    for progress in &tool.progress {
        match progress {
            ToolProgress::Update(text) => agent.emit(&json!({
                "type": "tool_execution_update", "toolCallId": tool.id, "toolName": tool.name,
                "args": tool.args, "partialResult": {"content": wire::text_content(text)}
            })),
            ToolProgress::Gate(name) => {
                if let Waited::Aborted = agent.gate(name, abort).await {
                    return ToolOutcome {
                        text: "Command aborted".into(),
                        is_error: true,
                        duration_ms: Some(elapsed()),
                    };
                }
            }
        }
    }
    ToolOutcome {
        text: tool.result.clone(),
        is_error: tool.is_error,
        duration_ms: Some(elapsed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_retryable() {
        assert!(is_retryable("503: service unavailable"));
        assert!(is_retryable("Rate limit exceeded"));
        assert!(!is_retryable("400: invalid request"));
    }
}
