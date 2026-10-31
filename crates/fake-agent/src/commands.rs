//! Handling stdin commands, with pi's response shapes and error texts.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::agent::{Agent, QueueMode, RunControl};
use crate::ids::uuid;
use crate::session::Session;
use crate::wire;

/// pi's rejection of a plain prompt while a run is active.
const BUSY: &str = "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.";

/// Commands pi has that the fake does not implement.
const UNSUPPORTED: &[&str] = &[
    "switch_session",
    "fork",
    "clone",
    "get_fork_messages",
    "get_tree",
    "export_html",
    "bash",
    "abort_bash",
    "abort_retry",
    "cycle_model",
];

/// Handle one stdin line. Commands that must wait (abort, extension
/// commands, compact, new_session) continue in their own task.
pub fn handle_line(agent: &Arc<Agent>, line: &str) {
    let command: Value = match serde_json::from_str(line) {
        Ok(command) => command,
        Err(err) => {
            let message = format!("Failed to parse command: {err}");
            agent.emit(&wire::error(None, "parse", &message));
            return;
        }
    };
    let id = command["id"].as_str().map(str::to_owned);
    let id = id.as_deref();
    let kind = command["type"].as_str().unwrap_or_default();
    let respond = |data: Option<Value>| agent.emit(&wire::ok(id, kind, data));
    let fail = |message: &str| agent.emit(&wire::error(id, kind, message));
    let text = command["message"].as_str().unwrap_or_default();
    match kind {
        "prompt" => prompt(agent, id, text, command["streamingBehavior"].as_str()),
        "steer" | "follow_up" => {
            enqueue(agent, kind == "steer", text);
            respond(Some(json!({"disposition": "queued"})));
        }
        "abort" => {
            let aborting = request_abort(agent);
            let agent = agent.clone();
            let id = id.map(str::to_owned);
            tokio::spawn(async move {
                settled(aborting).await;
                agent.emit(&wire::ok(id.as_deref(), "abort", None));
            });
        }
        "clear_queue" => {
            let (steering, follow_up) = {
                let mut state = agent.state();
                (
                    std::mem::take(&mut state.steering),
                    std::mem::take(&mut state.follow_up),
                )
            };
            agent.emit(&agent.queue_update());
            respond(Some(json!({"steering": steering, "followUp": follow_up})));
        }
        "extension_ui_response" => {
            let answer = agent.state().dialogs.remove(id.unwrap_or_default());
            if let Some(answer) = answer {
                let _ = answer.send(command);
            }
        }
        "new_session" => new_session(agent, id, command["parentSession"].as_str()),
        "compact" => compact(agent, id),
        "get_state" => respond(Some(state(agent))),
        "get_messages" => {
            respond(Some(json!({"messages": agent.state().session.messages()})));
        }
        "get_entries" => {
            let state = agent.state();
            match state.session.entries_since(command["since"].as_str()) {
                Some(entries) => {
                    let data = json!({"entries": entries, "leafId": state.session.leaf_id()});
                    drop(state);
                    respond(Some(data));
                }
                None => {
                    drop(state);
                    let since = command["since"].as_str().unwrap_or_default();
                    fail(&format!("Entry not found: {since}"));
                }
            }
        }
        "get_last_assistant_text" => respond(Some(json!({"text": last_assistant_text(agent)}))),
        "get_session_stats" => respond(Some(session_stats(agent))),
        "get_commands" => respond(Some(commands(agent))),
        "get_available_models" => {
            let models: Vec<Value> = agent.scenario.models.iter().map(wire::model).collect();
            respond(Some(json!({"models": models})));
        }
        "get_available_thinking_levels" => {
            respond(Some(json!({"levels": thinking_levels(agent)})));
        }
        "set_model" => {
            let provider = command["provider"].as_str().unwrap_or_default();
            let model_id = command["modelId"].as_str().unwrap_or_default();
            let found = agent
                .scenario
                .models
                .iter()
                .position(|m| m.provider == provider && m.id == model_id);
            match found {
                Some(index) => {
                    agent.state().model = index;
                    agent.persist(
                        json!({"type": "model_change", "provider": provider, "modelId": model_id}),
                    );
                    respond(Some(wire::model(&agent.scenario.models[index])));
                }
                None => fail(&format!("Model not found: {provider}/{model_id}")),
            }
        }
        "set_thinking_level" => {
            set_thinking_level(agent, command["level"].as_str().unwrap_or_default());
            respond(None);
        }
        "cycle_thinking_level" => {
            let levels = thinking_levels(agent);
            let current = agent.state().thinking_level.clone();
            let next = levels
                .iter()
                .position(|l| *l == current)
                .map_or(0, |i| (i + 1) % levels.len());
            let level = levels[next];
            set_thinking_level(agent, level);
            respond(Some(json!({"level": level})));
        }
        "set_steering_mode" | "set_follow_up_mode" => {
            match QueueMode::parse(command["mode"].as_str().unwrap_or_default()) {
                Some(mode) => {
                    let mut state = agent.state();
                    if kind == "set_steering_mode" {
                        state.steering_mode = mode;
                    } else {
                        state.follow_up_mode = mode;
                    }
                    drop(state);
                    respond(None);
                }
                None => fail("Invalid mode"),
            }
        }
        "set_auto_compaction" | "set_auto_retry" => {
            let enabled = command["enabled"].as_bool().unwrap_or(false);
            let mut state = agent.state();
            if kind == "set_auto_retry" {
                state.auto_retry = enabled;
            } else {
                state.auto_compaction = enabled;
            }
            drop(state);
            respond(None);
        }
        "set_session_name" => {
            let name = command["name"].as_str().unwrap_or_default().to_owned();
            agent.persist(json!({"type": "session_info", "name": name}));
            agent.state().session.name = Some(name.clone());
            agent.emit(&json!({"type": "session_info_changed", "name": name}));
            respond(None);
        }
        other if UNSUPPORTED.contains(&other) => {
            fail(&format!("Not supported by fake-agent: {other}"))
        }
        other => fail(&format!("Unknown command: {other}")),
    }
}

fn enqueue(agent: &Agent, steer: bool, text: &str) {
    {
        let mut state = agent.state();
        let queue = if steer {
            &mut state.steering
        } else {
            &mut state.follow_up
        };
        queue.push_back(text.to_owned());
    }
    agent.emit(&agent.queue_update());
}

fn prompt(agent: &Arc<Agent>, id: Option<&str>, text: &str, behavior: Option<&str>) {
    if let Some(name) = text.strip_prefix('/')
        && let Some(command) = agent
            .scenario
            .commands
            .iter()
            .find(|c| Some(c.name.as_str()) == name.split_whitespace().next())
    {
        let agent = agent.clone();
        let steps = command.steps.clone();
        let id = id.map(str::to_owned);
        tokio::spawn(async move {
            agent.side_steps(&steps).await;
            let data = json!({"disposition": "handled"});
            agent.emit(&wire::ok(id.as_deref(), "prompt", Some(data)));
        });
        return;
    }
    if agent.state().run.is_some() {
        match behavior {
            Some("steer") => enqueue(agent, true, text),
            Some("followUp") => enqueue(agent, false, text),
            _ => return agent.emit(&wire::error(id, "prompt", BUSY)),
        }
        return agent.emit(&wire::ok(
            id,
            "prompt",
            Some(json!({"disposition": "queued"})),
        ));
    }
    agent.emit(&wire::ok(
        id,
        "prompt",
        Some(json!({"disposition": "started"})),
    ));
    let (abort_tx, abort_rx) = watch::channel(false);
    let (done_tx, done_rx) = watch::channel(false);
    agent.state().run = Some(RunControl {
        abort: abort_tx,
        done: done_rx,
    });
    tokio::spawn(crate::run::run(
        agent.clone(),
        text.to_owned(),
        abort_rx,
        done_tx,
    ));
}

/// Signal the active run (if any) to abort. This happens while the command
/// is handled, so later commands already see the run as aborting.
fn request_abort(agent: &Agent) -> Option<watch::Receiver<bool>> {
    let state = agent.state();
    state.run.as_ref().map(|run| {
        run.abort.send_replace(true);
        run.done.clone()
    })
}

/// Wait until an aborted run settled.
async fn settled(done: Option<watch::Receiver<bool>>) {
    if let Some(mut done) = done {
        let _ = done.wait_for(|done| *done).await;
    }
}

fn set_thinking_level(agent: &Agent, level: &str) {
    agent.state().thinking_level = level.to_owned();
    agent.persist(json!({"type": "thinking_level_change", "thinkingLevel": level}));
    agent.emit(&json!({"type": "thinking_level_changed", "level": level}));
}

fn thinking_levels(agent: &Agent) -> Vec<&'static str> {
    if agent.model().reasoning {
        vec!["off", "minimal", "low", "medium", "high"]
    } else {
        vec!["off"]
    }
}

fn state(agent: &Agent) -> Value {
    let model = wire::model(&agent.model());
    let state = agent.state();
    let mut data = json!({
        "model": model,
        "thinkingLevel": state.thinking_level,
        "isStreaming": state.run.is_some(),
        "isCompacting": state.compacting,
        "steeringMode": state.steering_mode.as_str(),
        "followUpMode": state.follow_up_mode.as_str(),
        "sessionId": state.session.id,
        "autoCompactionEnabled": state.auto_compaction,
        "messageCount": state.session.messages().len(),
        "pendingMessageCount": state.steering.len() + state.follow_up.len(),
    });
    if let Some(file) = state.session.file() {
        data["sessionFile"] = file.display().to_string().into();
    }
    if let Some(name) = &state.session.name {
        data["sessionName"] = name.clone().into();
    }
    data
}

fn last_assistant_text(agent: &Agent) -> Option<String> {
    let state = agent.state();
    let message = state
        .session
        .messages()
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")?;
    let text: String = message["content"]
        .as_array()?
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    (!text.is_empty()).then_some(text)
}

fn session_stats(agent: &Agent) -> Value {
    let model = agent.model();
    let state = agent.state();
    let messages = state.session.messages();
    let count = |role: &str| messages.iter().filter(|m| m["role"] == role).count();
    let assistants = || messages.iter().filter(|m| m["role"] == "assistant");
    let sum = |field: &str| -> u64 {
        assistants()
            .filter_map(|m| m["usage"][field].as_u64())
            .sum()
    };
    let tool_calls = assistants()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .filter(|b| b["type"] == "toolCall")
        .count();
    let cost: f64 = assistants()
        .filter_map(|m| m["usage"]["cost"]["total"].as_f64())
        .sum();
    let context = assistants()
        .next_back()
        .and_then(|m| m["usage"]["totalTokens"].as_u64());
    let (input, output) = (sum("input"), sum("output"));
    let mut data = json!({
        "sessionId": state.session.id,
        "userMessages": count("user"),
        "assistantMessages": count("assistant"),
        "toolCalls": tool_calls,
        "toolResults": count("toolResult"),
        "totalMessages": messages.len(),
        "tokens": {"input": input, "output": output, "cacheRead": 0, "cacheWrite": 0,
                   "total": input + output},
        "cost": cost,
        "contextUsage": {
            "tokens": context,
            "contextWindow": model.context_window,
            "percent": context.map(|t| t as f64 * 100.0 / model.context_window as f64),
        },
    });
    if let Some(file) = state.session.file() {
        data["sessionFile"] = file.display().to_string().into();
    }
    data
}

fn commands(agent: &Agent) -> Value {
    let commands: Vec<Value> = agent
        .scenario
        .commands
        .iter()
        .map(|c| {
            let mut command = json!({
                "name": c.name,
                "source": "extension",
                "sourceInfo": {"path": "<fake-agent scenario>", "source": "cli",
                               "scope": "temporary", "origin": "top-level"},
            });
            if let Some(description) = &c.description {
                command["description"] = description.clone().into();
            }
            command
        })
        .collect();
    json!({"commands": commands})
}

fn new_session(agent: &Arc<Agent>, id: Option<&str>, parent: Option<&str>) {
    let aborting = request_abort(agent);
    let agent = agent.clone();
    let id = id.map(str::to_owned);
    let parent = parent.map(str::to_owned);
    tokio::spawn(async move {
        settled(aborting).await;
        let model = agent.model();
        let session = {
            let state = agent.state();
            Session::create(
                agent.sessions_dir.as_deref(),
                uuid(),
                &agent.cwd,
                parent.as_deref(),
                (&model.provider, &model.id),
                &state.thinking_level,
            )
        };
        agent.state().session = session;
        agent.side_steps(&agent.scenario.new_session).await;
        let data = json!({"cancelled": false});
        agent.emit(&wire::ok(id.as_deref(), "new_session", Some(data)));
    });
}

/// pi's error text for a session with nothing to summarize.
const NOTHING_TO_COMPACT: &str = "Nothing to compact (session too small)";

fn compact(agent: &Arc<Agent>, id: Option<&str>) {
    let aborting = request_abort(agent);
    let agent = agent.clone();
    let id = id.map(str::to_owned);
    tokio::spawn(async move {
        settled(aborting).await;
        let id = id.as_deref();
        agent.state().compacting = true;
        agent.emit(&json!({"type": "compaction_start", "reason": "manual"}));
        let outcome = summarize(&agent);
        agent.state().compacting = false;
        match outcome {
            Ok(result) => {
                agent.emit(&json!({
                    "type": "compaction_end", "reason": "manual", "result": result,
                    "aborted": false, "willRetry": false
                }));
                agent.emit(&wire::ok(id, "compact", Some(result)));
            }
            Err(error) => {
                agent.emit(&json!({
                    "type": "compaction_end", "reason": "manual", "aborted": false,
                    "willRetry": false, "errorMessage": format!("Compaction failed: {error}")
                }));
                agent.emit(&wire::error(id, "compact", &error));
            }
        }
    });
}

/// Summarize with the next model reply and persist a compaction entry.
fn summarize(agent: &Agent) -> Result<Value, String> {
    let first_kept = {
        let state = agent.state();
        let users = state
            .session
            .entries_since(None)
            .unwrap_or_default()
            .iter()
            .filter(|e| e["type"] == "message" && e["message"]["role"] == "user")
            .filter_map(|e| e["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        users.last().cloned().ok_or(NOTHING_TO_COMPACT)?
    };
    let reply = agent.next_reply();
    if let Some(error) = reply.error {
        return Err(error);
    }
    let summary: String = reply
        .content
        .iter()
        .filter_map(|step| match step {
            crate::scenario::Step::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    let usage = reply.usage.unwrap_or_default();
    let tokens_before = agent
        .state()
        .session
        .messages()
        .iter()
        .rev()
        .find_map(|m| m["usage"]["totalTokens"].as_u64())
        .unwrap_or(0);
    let entry = agent.persist(json!({
        "type": "compaction", "summary": summary, "firstKeptEntryId": first_kept,
        "tokensBefore": tokens_before
    }));
    Ok(json!({
        "summary": entry["summary"],
        "firstKeptEntryId": first_kept,
        "tokensBefore": tokens_before,
        "estimatedTokensAfter": usage.output,
        "usage": wire::usage(Some(usage)),
        "details": {"readFiles": [], "modifiedFiles": []},
    }))
}
