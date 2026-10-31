//! The recorded scenarios. Each drives real pi against the mock model.
//!
//! Drivers only send a command after observing the event that makes it
//! meaningful (or after the mock model reached a gate), so recordings are
//! reproducible without sleeps.

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use crate::env::{MAX_TOKENS, MODEL_B, PROVIDER};
use crate::mock_model::{Finish, Part, Reply, chunks};
use crate::rig::{PiProcess, path_str, session_file};
use crate::scenario::{Ctx, Scenario};

/// A fixed session id for `--session-id`, so the session file name is stable.
const SESSION_ID: &str = "0190f0a0-0000-7000-8000-000000000001";

macro_rules! scenario {
    ($name:literal, $about:literal, $replies:expr, $driver:ident) => {
        Scenario {
            name: $name,
            about: $about,
            replies: $replies,
            driver: |ctx| Box::pin($driver(ctx)),
        }
    };
}

/// Every scenario, in README order.
pub fn all() -> Vec<Scenario> {
    vec![
        scenario!(
            "plain-text",
            "One prompt answered with streamed text.",
            || vec![Reply::text("Hello from the mock model.")],
            plain_text
        ),
        scenario!(
            "thinking",
            "Reasoning deltas followed by text.",
            || vec![Reply::thinking(
                "Let me think about it.",
                "The answer is 42."
            )],
            single_prompt
        ),
        scenario!(
            "tool-calls",
            "bash, read and edit tool calls in successive turns, then text.",
            || vec![
                Reply::tool_call("call_bash", "bash", json!({"command": "ls"})),
                Reply::tool_call("call_read", "read", json!({"path": "notes.txt"})),
                Reply::tool_call(
                    "call_edit",
                    "edit",
                    json!({"path": "notes.txt", "edits": [{"oldText": "beta", "newText": "BETA"}]}),
                ),
                Reply::tool_call("call_fail", "read", json!({"path": "missing.txt"})),
                Reply::text("Done with the tools."),
            ],
            single_prompt
        ),
        scenario!(
            "nested-tool",
            "An extension tool that runs `read` through ctx.executeTool (parentToolCallId).",
            || vec![
                Reply::tool_call(
                    "call_nested",
                    "fixture_nested",
                    json!({"path": "notes.txt"})
                ),
                Reply::text("Nested read finished."),
            ],
            single_prompt
        ),
        scenario!(
            "parallel-tools",
            "Two tool calls in one assistant message.",
            || vec![
                Reply::stream(
                    vec![
                        Part::ToolCall {
                            id: "call_a".into(),
                            name: "read".into(),
                            args: json!({"path": "notes.txt"}),
                        },
                        Part::ToolCall {
                            id: "call_b".into(),
                            name: "read".into(),
                            args: json!({"path": "src/main.txt"}),
                        },
                    ],
                    Finish::ToolCalls,
                ),
                Reply::text("Read both files."),
            ],
            single_prompt
        ),
        scenario!(
            "steer",
            "steer while text streams; delivered as a new turn before the run ends.",
            || vec![
                Reply::gated_text("Working on it", "g1", " and done."),
                Reply::text("Steered answer."),
            ],
            steer
        ),
        scenario!(
            "steer-during-tool",
            "steer while a tool runs; delivered after the tool, before the next model call.",
            || vec![
                Reply::tool_call("call_gate", "bash", json!({"command": "cat fifo"})),
                Reply::text("Saw the steer."),
            ],
            steer_during_tool
        ),
        scenario!(
            "follow-up",
            "prompt without streamingBehavior while busy (rejected), then follow_up and prompt{followUp}.",
            || vec![
                Reply::gated_text("First answer", "g1", " finished."),
                Reply::text("Follow-up answer."),
                Reply::text("Second follow-up answer."),
            ],
            follow_up
        ),
        scenario!(
            "clear-queue",
            "Queue a steer and a follow-up, then clear_queue.",
            || vec![Reply::gated_text("Busy", "g1", " done.")],
            clear_queue
        ),
        scenario!(
            "queue-when-idle",
            "steer, follow_up and prompt{streamingBehavior} sent while idle.",
            || vec![
                Reply::text("Answer to the idle prompt."),
                Reply::text("Answer to the idle follow-up."),
            ],
            queue_when_idle
        ),
        scenario!(
            "abort-text",
            "abort while text streams.",
            || vec![Reply::gated_text("Partial text", "never", " unreachable.")],
            abort_text
        ),
        scenario!(
            "abort-tool",
            "abort while a bash tool runs.",
            || vec![Reply::tool_call(
                "call_sleep",
                "bash",
                json!({"command": "sleep 60"})
            )],
            abort_tool
        ),
        scenario!(
            "abort-then-prompt",
            "abort, then a new prompt works.",
            || vec![
                Reply::gated_text("Partial", "never", " unreachable."),
                Reply::text("Fresh answer."),
            ],
            abort_then_prompt
        ),
        scenario!(
            "abort-with-queue",
            "abort while a follow-up is queued; the follow-up stays queued for the next prompt.",
            || vec![
                Reply::gated_text("Partial", "never", " unreachable."),
                Reply::text("Answer to the next prompt."),
                Reply::text("Answer to the queued follow-up."),
            ],
            abort_with_queue
        ),
        scenario!("abort-idle", "abort while idle.", Vec::new, abort_idle),
        scenario!(
            "abort-dialog",
            "abort while a permission dialog is open: abort answers only after the dialog does.",
            || vec![Reply::tool_call(
                "call_guarded",
                "bash",
                json!({"command": "echo fixture-guard"}),
            )],
            abort_dialog
        ),
        scenario!(
            "error-stop",
            "A non-retryable HTTP 400: stopReason error, no retry.",
            || vec![Reply::error(
                400,
                "invalid request: mock rejects this prompt"
            )],
            single_prompt
        ),
        scenario!(
            "length-stop",
            "finish_reason length at maxTokens: stopReason length.",
            || vec![
                Reply::stream(
                    vec![Part::Text(chunks("This answer is cut", 8))],
                    Finish::Length
                )
                .with_completion_tokens(MAX_TOKENS),
            ],
            single_prompt
        ),
        scenario!(
            "retry-success",
            "HTTP 503 once, then success (auto_retry_start/end).",
            || vec![
                Reply::error(503, "service unavailable"),
                Reply::text("Recovered after a retry."),
            ],
            single_prompt
        ),
        scenario!(
            "retry-exhausted",
            "HTTP 503 until retries run out (maxRetries 2).",
            || vec![
                Reply::error(503, "service unavailable"),
                Reply::error(503, "service unavailable"),
                Reply::error(503, "service unavailable"),
            ],
            single_prompt
        ),
        scenario!(
            "compact",
            "compact on an empty session (fails), then two exchanges and a successful compact.",
            || vec![
                Reply::text("Something worth summarizing."),
                Reply::text("Something else."),
                Reply::text("Summary: the user asked for something worth summarizing."),
            ],
            compact
        ),
        scenario!(
            "extension-dialogs",
            "Extension command with select/confirm/input/editor, notify, setStatus, setWidget, setTitle, appendEntry.",
            Vec::new,
            extension_dialogs
        ),
        scenario!(
            "extension-dialog-timeout",
            "A select dialog with a timeout that expires unanswered; a late answer is ignored.",
            Vec::new,
            extension_dialog_timeout
        ),
        scenario!(
            "permission-dialog",
            "A tool_call hook asks with select mid-run; one call allowed, one blocked.",
            || vec![
                Reply::tool_call(
                    "call_allow",
                    "bash",
                    json!({"command": "echo fixture-guard ok"})
                ),
                Reply::tool_call(
                    "call_block",
                    "bash",
                    json!({"command": "echo fixture-guard no"})
                ),
                Reply::text("Permission scenario done."),
            ],
            permission_dialog
        ),
        scenario!(
            "session-resume",
            "Create a session with --session-id, exit, then resume it with --session <abs>.",
            || vec![
                Reply::text("First run answer."),
                Reply::text("Resumed answer.")
            ],
            session_resume
        ),
        scenario!(
            "new-session",
            "new_session with parentSession, then a prompt in the new session.",
            || vec![
                Reply::text("Old session answer."),
                Reply::text("New session answer.")
            ],
            new_session
        ),
        scenario!(
            "state-queries",
            "get_state, get_commands, get_session_stats, get_messages, get_entries, models, unknown command, bad JSON.",
            || vec![Reply::text("Answer for the stats.")],
            state_queries
        ),
        scenario!(
            "model-thinking",
            "set_model, set_thinking_level, cycle_thinking_level, set_session_name.",
            || vec![Reply::text("Answer from model b.")],
            model_thinking
        ),
        scenario!(
            "rpc-bash",
            "The RPC bash command with bash_execution_update.",
            Vec::new,
            rpc_bash
        ),
        scenario!(
            "crash-sigkill",
            "SIGKILL pi while text streams.",
            || vec![Reply::gated_text("Streaming before the crash", "never", "")],
            crash_sigkill
        ),
        scenario!("sighup-idle", "SIGHUP an idle pi.", Vec::new, sighup_idle),
        scenario!(
            "sighup-running",
            "SIGHUP pi while text streams.",
            || vec![Reply::gated_text(
                "Streaming before the hangup",
                "never",
                ""
            )],
            sighup_running
        ),
        scenario!(
            "sigterm-idle",
            "SIGTERM an idle pi.",
            Vec::new,
            sigterm_idle
        ),
        scenario!(
            "stdin-close-running",
            "Close pi's stdin while text streams.",
            || vec![Reply::gated_text(
                "Streaming before stdin closes",
                "never",
                ""
            )],
            stdin_close_running
        ),
    ]
}

fn prompt(message: &str) -> Value {
    json!({"type": "prompt", "message": message})
}

fn ensure_success(response: &Value) -> Result<()> {
    ensure!(response["success"] == true, "command failed: {response}");
    Ok(())
}

/// Wait until the stream is visibly running (first text delta).
async fn wait_text_delta(pi: &mut PiProcess) -> Result<Value> {
    pi.wait_out("text_delta", |v| {
        v["type"] == "message_update" && v["assistantMessageEvent"]["type"] == "text_delta"
    })
    .await
}

async fn finish(ctx: &mut Ctx, mut pi: PiProcess) -> Result<()> {
    pi.close().await?;
    ctx.keep(&pi);
    Ok(())
}

async fn plain_text(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    let response = pi.call("1", prompt("Say hello.")).await?;
    ensure_success(&response)?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn single_prompt(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Do the scripted thing.")).await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn steer(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Start working.")).await?;
    wait_text_delta(&mut pi).await?;
    let response = pi
        .call("2", json!({"type": "steer", "message": "Change of plan."}))
        .await?;
    ensure_success(&response)?;
    ctx.release(&pi, "g1").await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn steer_during_tool(ctx: &mut Ctx) -> Result<()> {
    // The bash tool blocks reading a FIFO until the recorder writes to it.
    let fifo = ctx.env.work_dir().join("fifo");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    ensure!(status.success(), "mkfifo failed");
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Run the tool.")).await?;
    // bash reports an empty update once its process runs; waiting for it
    // pins the order of that update and the next command.
    pi.wait_event("tool_execution_update").await?;
    pi.call(
        "2",
        json!({"type": "steer", "message": "Also mention the steer."}),
    )
    .await?;
    pi.meta(json!({"fixture": "release_gate", "gate": "tool"}));
    tokio::fs::write(&fifo, "tool output\n").await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn follow_up(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("First task.")).await?;
    wait_text_delta(&mut pi).await?;
    let rejected = pi.call("2", prompt("Prompt while busy.")).await?;
    ensure!(
        rejected["success"] == false,
        "expected a rejection: {rejected}"
    );
    pi.call(
        "3",
        json!({"type": "follow_up", "message": "Then do this."}),
    )
    .await?;
    pi.call(
        "4",
        json!({"type": "prompt", "message": "And this.", "streamingBehavior": "followUp"}),
    )
    .await?;
    ctx.release(&pi, "g1").await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn clear_queue(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Busy task.")).await?;
    wait_text_delta(&mut pi).await?;
    pi.call("2", json!({"type": "steer", "message": "Queued steer."}))
        .await?;
    pi.call(
        "3",
        json!({"type": "follow_up", "message": "Queued follow-up."}),
    )
    .await?;
    let cleared = pi.call("4", json!({"type": "clear_queue"})).await?;
    ensure_success(&cleared)?;
    ctx.release(&pi, "g1").await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn queue_when_idle(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", json!({"type": "steer", "message": "Idle steer."}))
        .await?;
    pi.call(
        "2",
        json!({"type": "follow_up", "message": "Idle follow-up."}),
    )
    .await?;
    let response = pi
        .call(
            "3",
            json!({"type": "prompt", "message": "Idle prompt.", "streamingBehavior": "steer"}),
        )
        .await?;
    ensure!(
        response["data"]["disposition"] == "started",
        "unexpected: {response}"
    );
    pi.wait_event("agent_settled").await?;
    pi.call("4", json!({"type": "get_state"})).await?;
    finish(ctx, pi).await
}

async fn abort_with_queue(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Long task.")).await?;
    wait_text_delta(&mut pi).await?;
    pi.call(
        "2",
        json!({"type": "follow_up", "message": "Queued follow-up."}),
    )
    .await?;
    pi.call("3", json!({"type": "abort"})).await?;
    pi.call("4", json!({"type": "get_state"})).await?;
    pi.call("5", prompt("Next prompt.")).await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn abort_idle(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    let response = pi.call("1", json!({"type": "abort"})).await?;
    ensure_success(&response)?;
    finish(ctx, pi).await
}

async fn abort_dialog(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Run the guarded command.")).await?;
    let id = wait_dialog(&mut pi, "select").await?;
    // pi's abort waits for idle, and the open dialog keeps the tool hook
    // pending: get_state answers while abort does not.
    pi.send(json!({"id": "2", "type": "abort"})).await?;
    pi.call("3", json!({"type": "get_state"})).await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "value": "Allow"}))
        .await?;
    pi.wait_response("2").await?;
    finish(ctx, pi).await
}

async fn abort_text(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Write a long answer.")).await?;
    wait_text_delta(&mut pi).await?;
    ctx.mock.wait_reached("never", crate::rig::WAIT).await?;
    let response = pi.call("2", json!({"type": "abort"})).await?;
    ensure_success(&response)?;
    finish(ctx, pi).await
}

async fn abort_tool(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Run a slow command.")).await?;
    // bash reports an empty update once its process runs; waiting for it
    // pins the order of that update and the next command.
    pi.wait_event("tool_execution_update").await?;
    let response = pi.call("2", json!({"type": "abort"})).await?;
    ensure_success(&response)?;
    finish(ctx, pi).await
}

async fn abort_then_prompt(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Write a long answer.")).await?;
    ctx.mock.wait_reached("never", crate::rig::WAIT).await?;
    wait_text_delta(&mut pi).await?;
    pi.call("2", json!({"type": "abort"})).await?;
    pi.call("3", prompt("Try again.")).await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn compact(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    let empty = pi.call("0", json!({"type": "compact"})).await?;
    ensure!(empty["success"] == false, "expected a failure: {empty}");
    pi.call("1", prompt("Tell me something.")).await?;
    pi.wait_event("agent_settled").await?;
    pi.call("1b", prompt("Tell me more.")).await?;
    pi.wait_event("agent_settled").await?;
    let response = pi
        .call(
            "2",
            json!({"type": "compact", "customInstructions": "Keep it short."}),
        )
        .await?;
    ensure_success(&response)?;
    pi.call("3", json!({"type": "get_session_stats"})).await?;
    finish(ctx, pi).await
}

/// Wait for the next dialog request and return its id.
async fn wait_dialog(pi: &mut PiProcess, method: &str) -> Result<String> {
    let request = pi
        .wait_out(method, |v| {
            v["type"] == "extension_ui_request" && v["method"] == method
        })
        .await?;
    Ok(request["id"].as_str().unwrap_or_default().to_owned())
}

async fn extension_dialogs(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.send(json!({"id": "1", "type": "prompt", "message": "/fixture-dialogs"}))
        .await?;
    let id = wait_dialog(&mut pi, "select").await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "value": "green"}))
        .await?;
    let id = wait_dialog(&mut pi, "confirm").await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "confirmed": true}))
        .await?;
    let id = wait_dialog(&mut pi, "input").await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "value": "alice"}))
        .await?;
    let id = wait_dialog(&mut pi, "editor").await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "cancelled": true}))
        .await?;
    let response = pi.wait_response("1").await?;
    ensure_success(&response)?;
    pi.call("2", json!({"type": "get_entries"})).await?;
    finish(ctx, pi).await
}

async fn extension_dialog_timeout(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.send(json!({"id": "1", "type": "prompt", "message": "/fixture-timeout"}))
        .await?;
    let id = wait_dialog(&mut pi, "select").await?;
    pi.wait_response("1").await?;
    // Answer after pi gave up: pi must ignore it.
    pi.send(json!({"type": "extension_ui_response", "id": id, "value": "yes"}))
        .await?;
    pi.call("2", json!({"type": "get_state"})).await?;
    finish(ctx, pi).await
}

async fn permission_dialog(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Run the guarded commands.")).await?;
    let id = wait_dialog(&mut pi, "select").await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "value": "Allow"}))
        .await?;
    let id = wait_dialog(&mut pi, "select").await?;
    pi.send(json!({"type": "extension_ui_response", "id": id, "value": "Block"}))
        .await?;
    pi.wait_event("agent_settled").await?;
    finish(ctx, pi).await
}

async fn session_resume(ctx: &mut Ctx) -> Result<()> {
    let mut first = ctx.spawn(&["--session-id", SESSION_ID])?;
    let state = first.call("1", json!({"type": "get_state"})).await?;
    first.call("2", prompt("Remember the word apple.")).await?;
    first.wait_event("agent_settled").await?;
    let after = first.call("3", json!({"type": "get_state"})).await?;
    let file = session_file(&after).or_else(|_| session_file(&state))?;
    finish(ctx, first).await?;

    let file = path_str(&file);
    let mut second = ctx.spawn(&["--session", &file])?;
    second.call("1", json!({"type": "get_state"})).await?;
    second.call("2", json!({"type": "get_messages"})).await?;
    second.call("3", prompt("What was the word?")).await?;
    second.wait_event("agent_settled").await?;
    second.call("4", json!({"type": "get_entries"})).await?;
    finish(ctx, second).await
}

async fn new_session(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("First session prompt.")).await?;
    pi.wait_event("agent_settled").await?;
    let state = pi.call("2", json!({"type": "get_state"})).await?;
    let parent = path_str(&session_file(&state)?);
    let response = pi
        .call("3", json!({"type": "new_session", "parentSession": parent}))
        .await?;
    ensure_success(&response)?;
    pi.call("4", json!({"type": "get_state"})).await?;
    pi.call("5", prompt("Second session prompt.")).await?;
    pi.wait_event("agent_settled").await?;
    pi.call("6", json!({"type": "get_state"})).await?;
    finish(ctx, pi).await
}

async fn state_queries(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", json!({"type": "get_state"})).await?;
    pi.call("2", json!({"type": "get_commands"})).await?;
    pi.call("3", json!({"type": "get_available_models"}))
        .await?;
    pi.call("4", json!({"type": "get_available_thinking_levels"}))
        .await?;
    pi.call("5", prompt("Answer something.")).await?;
    pi.wait_event("agent_settled").await?;
    pi.call("6", json!({"type": "get_session_stats"})).await?;
    pi.call("7", json!({"type": "get_messages"})).await?;
    let entries = pi.call("8", json!({"type": "get_entries"})).await?;
    if let Some(first) = entries["data"]["entries"][0]["id"].as_str() {
        pi.call("9", json!({"type": "get_entries", "since": first}))
            .await?;
    }
    pi.call(
        "10",
        json!({"type": "get_entries", "since": "no-such-entry"}),
    )
    .await?;
    pi.call("11", json!({"type": "get_last_assistant_text"}))
        .await?;
    pi.call("12", json!({"type": "no_such_command"})).await?;
    pi.send_raw("{not json").await?;
    pi.wait_out("parse error", |v| v["command"] == "parse")
        .await?;
    finish(ctx, pi).await
}

async fn model_thinking(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    let response = pi
        .call(
            "1",
            json!({"type": "set_model", "provider": PROVIDER, "modelId": MODEL_B}),
        )
        .await?;
    ensure_success(&response)?;
    pi.call("2", json!({"type": "set_thinking_level", "level": "high"}))
        .await?;
    pi.call("3", json!({"type": "cycle_thinking_level"}))
        .await?;
    pi.call(
        "4",
        json!({"type": "set_session_name", "name": "fixture session"}),
    )
    .await?;
    pi.call(
        "5",
        json!({"type": "set_model", "provider": PROVIDER, "modelId": "no-such-model"}),
    )
    .await?;
    pi.call("6", prompt("Which model are you?")).await?;
    pi.wait_event("agent_settled").await?;
    pi.call("7", json!({"type": "get_state"})).await?;
    finish(ctx, pi).await
}

async fn rpc_bash(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    let response = pi
        .call(
            "1",
            json!({"type": "bash", "command": "printf 'one\\ntwo\\n'"}),
        )
        .await?;
    ensure_success(&response)?;
    pi.call("2", json!({"type": "get_messages"})).await?;
    finish(ctx, pi).await
}

async fn crash_sigkill(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Start streaming.")).await?;
    ctx.mock.wait_reached("never", crate::rig::WAIT).await?;
    wait_text_delta(&mut pi).await?;
    pi.signal("KILL")?;
    pi.wait_exit().await?;
    ctx.keep(&pi);
    Ok(())
}

async fn sighup_idle(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", json!({"type": "get_state"})).await?;
    pi.signal("HUP")?;
    pi.wait_exit().await?;
    ctx.keep(&pi);
    Ok(())
}

async fn sigterm_idle(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", json!({"type": "get_state"})).await?;
    pi.signal("TERM")?;
    pi.wait_exit().await?;
    ctx.keep(&pi);
    Ok(())
}

async fn stdin_close_running(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Start streaming.")).await?;
    wait_text_delta(&mut pi).await?;
    finish(ctx, pi).await
}

async fn sighup_running(ctx: &mut Ctx) -> Result<()> {
    let mut pi = ctx.spawn(&[])?;
    pi.call("1", prompt("Start streaming.")).await?;
    ctx.mock.wait_reached("never", crate::rig::WAIT).await?;
    wait_text_delta(&mut pi).await?;
    pi.signal("HUP")?;
    pi.wait_exit().await?;
    ctx.keep(&pi);
    Ok(())
}
