# pi RPC fixtures

Sanitized recordings of real `pi --mode rpc` processes, one directory per pinned pi version
(`1.2.0/`). They are the ground truth for the PiDriver fold and for fake-agent's pi-RPC mode.

## How they are made

`crates/fixture-tool` runs real pi in an isolated temp dir (`HOME`, XDG dirs and
`PI_CODING_AGENT_DIR` all fresh; `--no-extensions --no-skills --no-prompt-templates --no-themes`)
against a **local mock model**: an OpenAI-compatible chat-completions server on loopback, wired in
through an isolated `models.json` (`api: "openai-completions"`, dummy key). No real provider or key is
ever used. The mock scripts text, reasoning, tool calls, HTTP errors, length stops and *gates* (a
stream holds until the recorder releases it), so every recording is driven by observed events, not
sleeps. A small test extension (`extension/fixture-ext.ts`, loaded with `-e`)
provides dialogs, fire-and-forget UI, `appendEntry`, a permission hook on bash commands containing
`fixture-guard`, and a tool that nests a `read` call.

Re-record (needs `pi` 1.2.0 on `PATH` or `$CONTROLROOM_PI`):

```bash
nix develop -c cargo run -p controlroom-fixture-tool -- record --raw-dir /tmp/pi-raw
rm -f fixtures/pi-rpc/1.2.0/*.jsonl
nix develop -c cargo run -p controlroom-fixture-tool -- sanitize /tmp/pi-raw fixtures/pi-rpc/1.2.0
nix develop -c cargo test -p controlroom-fixture-tool -p controlroom-fake-agent
```

## Format

One record per line, the transcript record shape: `{"seq", "ts", "dir", "raw"}`.

| `dir` | `raw` |
| --- | --- |
| `in` | a line written to pi's stdin |
| `out` | a line read from pi's stdout |
| `stderr` | a line read from pi's stderr |
| `exit` | `{"code": <i32\|null>, "signal": <i32\|null>}` |
| `meta` | recorder annotations, never seen by pi: `{"fixture":"env","root","baseUrl","args"}` (first record), `{"fixture":"release_gate","gate"}` (the mock model was let go), `{"fixture":"signal","signal"}`, `{"fixture":"close_stdin"}` |

The `meta` records let a replay reproduce the run: fake-agent's conformance test
(`crates/fake-agent/tests/conformance.rs`) sends the `in` lines at the same points, releases fake gates
of the same name, sends the same signals, and compares event signatures and the exit status.
`1.2.0/fake/<name>.json` is the fake-agent scenario that scripts the same model replies
(`<name>.N.json` for one process of a multi-process scenario).

Sanitizing (`crates/fixture-tool/src/sanitize.rs`) is textual, so untouched bytes stay exact: the temp
dir becomes `/home/alice/...` (`work/` → `/home/alice/project`, agent dir → `/home/alice/.pi/agent`),
the mock port becomes 4000, Nix store hashes are zeroed, UUIDs and 8-hex entry ids become sequential
placeholders (consistent across the files of one scenario), and all timestamps are rebased to start at
2023-11-14T22:13:20Z.

## Index (`1.2.0/`)

| Fixture | What it shows |
| --- | --- |
| `plain-text` | One prompt answered with streamed text. |
| `thinking` | Reasoning deltas followed by text. |
| `tool-calls` | bash, read and edit tool calls in successive turns (plus a failing read), then text. |
| `nested-tool` | An extension tool running `read` through `ctx.executeTool`: `parentToolCallId`, id `<parent>/1`. |
| `parallel-tools` | Two tool calls in one assistant message. |
| `steer` | `steer` while text streams; delivered as a new turn before the run ends. |
| `steer-during-tool` | `steer` while a tool runs; delivered after the tool, before the next model call. |
| `follow-up` | A plain `prompt` while busy (rejected), then `follow_up` and `prompt{streamingBehavior:"followUp"}`. |
| `clear-queue` | Queue a steer and a follow-up, then `clear_queue`. |
| `queue-when-idle` | `steer`, `follow_up` and `prompt{streamingBehavior}` sent while idle. |
| `abort-text` | `abort` while text streams. |
| `abort-tool` | `abort` while a bash tool runs. |
| `abort-then-prompt` | `abort`, then a new prompt works. |
| `abort-with-queue` | `abort` while a follow-up is queued; the follow-up waits for the next prompt. |
| `abort-idle` | `abort` while idle. |
| `abort-dialog` | `abort` while a permission dialog is open: abort answers only after the dialog does. |
| `error-stop` | A non-retryable HTTP 400: `stopReason: "error"`, no retry. |
| `length-stop` | `finish_reason: "length"` at `maxTokens`: `stopReason: "length"`. |
| `retry-success` | HTTP 503 once, then success (`auto_retry_start`/`auto_retry_end`). |
| `retry-exhausted` | HTTP 503 until retries run out (`retry.maxRetries` 2). |
| `compact` | `compact` on an empty session (fails), then two exchanges and a successful `compact`. |
| `extension-dialogs` | Extension command: select/confirm/input/editor, notify, setStatus, setWidget, setTitle, `appendEntry`. |
| `extension-dialog-timeout` | A select dialog whose timeout expires unanswered; a late answer is ignored. |
| `permission-dialog` | A `tool_call` hook asks with select mid-run; one call allowed, one blocked. |
| `session-resume.1`, `.2` | Create a session with `--session-id`, exit; resume it with `--session <abs file>`. |
| `new-session` | `new_session {parentSession}`, then a prompt in the new session. |
| `state-queries` | `get_state`, `get_commands`, models, thinking levels, `get_session_stats`, `get_messages`, `get_entries` (+ `since`, bad `since`), `get_last_assistant_text`, an unknown command, malformed JSON. |
| `model-thinking` | `set_model` (good and unknown), `set_thinking_level`, `cycle_thinking_level`, `set_session_name`. |
| `rpc-bash` | The RPC `bash` command with `bash_execution_update`. |
| `crash-sigkill` | SIGKILL while text streams. |
| `sighup-idle`, `sighup-running` | SIGHUP while idle / while text streams. |
| `sigterm-idle` | SIGTERM while idle. |
| `stdin-close-running` | stdin closed while text streams. |

fake-agent replays every fixture except `parallel-tools` (pi's tool end order is not deterministic)
and `rpc-bash` (fake-agent does not implement `bash`).

## What the recordings show (and where they contradict the spec or research)

References: spec = `docs/architecture.html`, research = `research/pi-rpc-and-extensions.md`.

1. **Abort outcome is not always `stopReason: "aborted"`** (contradicts spec §6.3 "aborted →
   cancelled" taken from the stopReason). Aborting during text gives `stopReason: "aborted"`
   (`errorMessage: "Request was aborted"`). Aborting during a tool ends the tool with
   `isError: true` ("Command aborted"), then pi starts **another turn** whose assistant message has
   `stopReason: "error"`, `errorMessage: "This operation was aborted"` (`abort-tool`,
   `abort-dialog`). Decide the run outcome from `agent_settled.aborted` first, then the stopReason
   (research §7.1's order).
2. **`abort` does not answer while an extension dialog is open** (`abort-dialog`). The dialog is not
   withdrawn; `abort` responds only after the dialog is answered, and the tool then ends with
   "Operation aborted". Other commands (`get_state`) are answered meanwhile. A driver that aborts
   must also answer (cancel) the open dialogs.
3. **A `system` message is part of the stream.** The first turn of a session emits
   `message_start`/`message_end` with `role: "system"` (prompt `sections`, `toolsAdded`) before the
   user message. It is persisted, counted in `messageCount`/`totalMessages`, returned by
   `get_messages`, and not re-sent after a resume. Research §1.2's message list omits it.
4. **`entry_appended` is not only for `appendEntry`** (research §1.2/§2 say "an extension appended a
   custom entry"). Every agent-level retry emits `entry_appended` with a `context_edit` entry that
   hides the failed attempt from the model context.
5. **Retry shape.** A retry is a new `agent_start` … `agent_end` inside the same settled run: failed
   message → `turn_end` → `agent_end{willRetry:true}` → `auto_retry_start` → `entry_appended` →
   `agent_start` → `turn_start` (no user message) → … `auto_retry_end{success:true}` comes between
   the successful `message_end` and its `turn_end`. On exhaustion: `agent_end{willRetry:false}` →
   `auto_retry_end{success:false, finalError}` → `agent_settled`.
6. **An errored request has no `message_update`s**; its `message_start` already carries
   `stopReason: "error"` and `errorMessage` (e.g. `"400: {...response body...}"`).
7. **`queue_update` precedes the command response** of `steer`, `follow_up` and queued prompts, and
   at delivery comes right after `turn_start`, before the queued user message.
8. **Steer and follow-up while idle are accepted and wait** (`queue-when-idle`): `disposition:
   "queued"`, no run starts. The next prompt delivers the steer in its first turn (right after the
   prompt's user message) and the follow-up as an extra turn. `prompt` with `streamingBehavior`
   while idle simply starts a run.
9. **`abort` keeps queued follow-ups** (`abort-with-queue`): they are not run after the abort, and
   are delivered after the next prompt's first turn.
10. **Signals and stdin.** SIGHUP exits with **code 129** (pi handles it; not a signal death), idle
    or mid-run, with no further events. SIGTERM exits with code 143. Closing stdin mid-run exits 0
    at once, with no `agent_end`/`agent_settled`. SIGKILL leaves the run open (signal 9).
11. **Dialogs.** No event marks a dialog timeout; a late `extension_ui_response` is silently ignored
    (`extension-dialog-timeout`). Dialogs raised by a `tool_call` hook appear between
    `tool_execution_start` and the first `tool_execution_update`.
12. **`durationMs` is absent from `tool_execution_end`** when the tool never ran (blocked by a hook,
    or aborted while its permission dialog was open).
13. **`new_session` re-runs extension `session_start` handlers** (twice here) before its response.
14. **Lazy session file.** `get_state.sessionFile` is reported before the file exists; `--session-id`
    with an unknown id prints a stderr warning and creates the session.
15. **Block end order is a provider artifact.** With `openai-completions`, all `thinking_end`,
    `text_end` and `toolcall_end` events come at the end of the stream, in content order, so a
    `thinking_end` can follow text deltas. With parallel tool calls, `tool_execution_end` follows
    completion order while the tool result messages keep call order.
16. **Length stops can turn into compaction** (from the source, not recorded): pi-ai's
    `isRecoverableLength` marks a `length` stop with `usage.output < maxTokens` for one
    compact-and-retry attempt. `length-stop` reports `output == maxTokens` to show the plain stop.
17. **Snapshots can be ahead.** A `message_start` for an assistant message may already contain
    streamed content (pi serializes a live object); rely on deltas and `message_end`.
18. **Small facts:** `set_model` emits no event; `set_thinking_level`/`cycle_thinking_level` emit
    `thinking_level_changed` before the response (cycling wraps `high` → `off`);
    `set_session_name` emits `session_info_changed`; a bad `get_entries` cursor fails with
    `Entry not found: <id>`; unknown commands fail with `Unknown command: <type>` and keep the `id`;
    malformed JSON gets a `parse` response without `id`; `abort` while idle succeeds; a failed
    `compact` emits `compaction_start` and `compaction_end{errorMessage}` before the failed response.
