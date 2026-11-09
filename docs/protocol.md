# controlroom protocol 1.0

The contract between the browser, the controller (`controlroom-web`), the agent daemon (`controlroom-agentd`), the host daemons (`controlroomd`) and the agent's extension (`@controlroom/pi`).
Source of truth: `crates/protocol` (Rust). TypeScript is generated (read-only for its users) into `web/src/lib/protocol/` (UI API) and `extensions/pi/src/protocol/` (agent API).
Spec: `docs/architecture.html` §3, §5, §7, §8, §8.1, §9, §11.4, §11.6, §12, §13, §22. Rationale: `docs/design/remote-tools.md`.

**Model.** pi runs only on the server, inside the agent daemon. Execution hosts run `controlroomd` in the host role: workspaces, named PTY shells, a tool executor with a journal, jobs, files, nono. A session is global and has one or more **workspaces** (`machine:path`); the agent addresses files as `machine:path`. The controller keeps sole custody of SSH keys and routes every host request.

Four APIs, all JSON-RPC 2.0 with newline-delimited framing (one compact JSON object per line, no batches):

| API | Between | Transport | Rust module | Version constant |
| --- | --- | --- | --- | --- |
| Agent daemon API | controller ↔ `controlroom-agentd` | local unix socket `/run/controlroom-agent/agentd.sock` | `agentd` | `AGENTD_PROTOCOL_VERSION` = 1.0 |
| Host API | controller ↔ `controlroomd` host role | SSH exec channel `controlroomd connect` | `host` | `HOST_PROTOCOL_VERSION` = 1.0 |
| UI API | browser ↔ controller | one WebSocket per tab | `ui` | `UI_PROTOCOL_VERSION` = 1.0 |
| Agent API | `@controlroom/pi` ↔ agent daemon | the session's unix socket `run/<id>.agent.sock` | `agent` | `AGENT_PROTOCOL_VERSION` = 1.0 |

Nothing is deployed, so the APIs are redefined in place as 1.0. §10 lists what the switch-over removed.

Rust areas (crate root modules): `ids`, `jsonrpc`, `error`, `version`, `session` (model), `timeline`, `agentd`, `host`, `agent`, `ui`, `typescript`.

## 1. Conventions

- Field and enum names are `snake_case`. Tagged unions use `kind` (or `op` for timeline ops and tool ops, `outcome` for results, `state` for connectivity and shell states).
- **Ids.** Store-assigned ids are validated ULID strings: `SessionId`, `WorkspaceId`, `DaemonId`, `QueueId`, `TodoId`, `JobId`, `GrantId`, `MessageId`, `OutboxId`, `SubscriptionId`, `ApprovalId`, `ControllerId`, `MachineId`, `KeyId`, `ProjectId`, `TaskId`, `ChecklistItemId`, `ScheduleId`, `Epoch`.
  Opaque string ids: `OpId` (client idempotency key; schedules use `<schedule id>:<scheduled ms>`), `DeviceId` (browser-generated), `ItemId` and `RequestId` (assigned deterministically by the transcript fold), `CallId` (pi's tool call id, or `<id>/<n>` for the steps of one tool), `ShellId` (`<session id>/<shell name>` on a host), `HostOpId` (a forwarded host request; the call id for `tool.run`).
- `RemotePath` is a POSIX string (may start with `~`, expanded only on the host). `UnixMillis`, `Seq` (transcript position, from 1) and `Rev` (feed or log revision, from 1) are numbers.
- Optional **params** fields may be omitted. Optional **result** fields are always present, as `null` when empty.
- No agent-specific payloads anywhere. Drivers translate pi's RPC into these types. Raw transcript lines appear only as opaque text in `RawFrame`, and only on request.

## 2. Versioning and tolerance

**Versioning.** `major.minor`. Peers are compatible iff the majors match; a mismatch is the `incompatible_version` error (a host shows `incompatible`).
A minor version only adds things an older peer can ignore or avoid:
new methods and notifications, new optional params fields, new result fields, new enum variants (including error kinds, timeline item kinds, request kinds, tool ops).
Everything else is a major change. The agent daemon and the controller deploy together on the server; host daemons upgrade on their own, so the host API is the one that must stay compatible across versions.

**Capabilities.** `hello.capabilities` lists optional features as free strings; unknown ones are kept. A client calls a feature's methods only when the peer advertised it.

| Peer | Capabilities |
| --- | --- |
| Host daemon | `shells` (`shell.*`, `bash` ops), `pty` (terminals attach with `controlroomd pty attach <shell id>`), `jobs` (`job_*` ops, `jobs.*`), `sandbox_approvals` (`approvals.subscribe`, `approval.respond`; without it, commands that need an approval are denied) |
| Agent daemon | `todos`, `jobs`, `subagents`, `sandbox_approvals` |
| Agent daemon → extension | `remote_tools` (`session.info`, `host.*`, `shell.*`; without it the extension leaves pi's built-in tools alone) |
| Controller | `schedules` |

**Tolerant decoding** (the prototype's "unknown variant hangs the call forever" bug, spec §16):

1. Every enum a peer *receives* has an `unknown` fallback (`#[serde(other)]`). A newer peer's variant decodes as `unknown` instead of failing the message.
   The raw tag is not kept (exceptions: capabilities are plain strings, and an unknown error keeps its `code` and `message`).
   Params-only enums (`FollowFrom`, `OptionValue`, `ProfileChange`, `ContentEncoding`) have none: an unknown value there is an `invalid_params` error for that call. `HostOp` is params but has `unknown`, so a host answers a newer op with `unsupported`.
2. Unknown fields are ignored everywhere.
3. `Message::parse` classifies a line by shape and keeps params and results raw. A response whose `result` doesn't parse fails **only that call** (`RpcResponse::into_typed::<M>()` → `invalid_response`).
   A malformed `error` member still resolves its id (`RpcError::from_value_lenient`), and malformed error `data` becomes `unknown`.
   A line that isn't a usable message returns `MessageError { id, error }` with the id when readable, so the client can fail that one call.
   Notifications that don't decode are dropped and logged; nothing closes the connection.
4. **Timeouts are client-side.** Every call has one: 30 s by default, longer for known slow methods (`machines.add_confirm`, `machines.upgrade`, `doctor.report`, `fs.list`). Tool calls are the exception: `tool.run` waits as long as the command runs (its timeout is enforced on the host) and the agent daemon applies the grace period (§6) instead.
   On expiry the client fails the call with `timeout` and drops a late response.

## 3. Session model (`session`, `agentd`)

**`SessionSummary`** (`session::SessionSummary`; every session, fed by `sessions.subscribe`, sent whole on each change; preview throttled to 1/s):
`id, name?, title?, workspaces[], shells[], created_at, updated_at, closed_at?, parent_id?, state, last_outcome?, last_error?, read_seq, last_turn_end_seq, unread, preview, attention?, rollup, context?, queue, requests[], todo?, live_work, options[], agent_live, sandbox, resumable`.
Sessions are global: no `machine_id`, no `cwd`; a session never goes stale with a machine, only its workspaces become unavailable.

- `workspaces[]` (`Workspace`): `{id, machine_id, path, root?, primary, added_at, availability}`. `path` as given, `root` resolved by the host (`null` until opened), the first is `primary` (relative paths and new shells default to it). `availability` = `{connectivity: Connectivity, since}`: the controller's link to that host and since when it has been in that state (a new `since` while connected means a new connection).
- `shells[]` (`Shell`): `{name, workspace_id, cwd, state, reset_pending, foreground?}`. `state`: `idle · running{call_id} · waiting_input{call_id} · gone`. `reset_pending`: the shell was reopened and lost its variables (the next result says so). `foreground`: the PTY's foreground process name, for the terminal's key pass-through. The UI shows one terminal tab per shell.
- `state`: `starting · idle · working · awaiting_permission · awaiting_input · failed · dormant · interrupted · closed`.
- `last_outcome` (`RunOutcome`): `completed · cancelled · failed · max_tokens · refusal · unknown`.
- `attention` (server-owned): `approval` (blocking request) › `question` (lighter tier) › `problem` (failed, interrupted, auto-continue paused) › `unread`. `rollup` counts the same over descendants.
- `queue` (`QueueStatus`): `len, paused, auto_send, send_now_pending`. The items are in the detail.
- `requests[]`: pending requests, including descendants' (`origin_child`).
- `todo`: `{done, total (excl. skipped), auto_continue: on|off|paused|stalled|budget}`. `live_work`: `{jobs, children, asks, host_calls}` (`host_calls`: tool calls waiting for an unreachable host or for their late result).
- `options[]`: neutral agent settings (`SessionOption {id, label, kind: select{value, choices} | bool{value}}`), e.g. model and thinking level.
- `agent_live`: a live, ready agent process is attached, so options can be changed and the agent's requests answered.
- `sandbox`: `{level: strict|relaxed|off, default_level, explicit, looser_than_default, grants[]}`; one level for the agent and every workspace. `context`: `{used, size?, cost?}`.

**`SessionDetail`** (one open session, from `session.subscribe`, replaced whole on change): `agent_sessions[]` (`{driver: pi|fake, native_id, started_at, ended_at?, started_by: new|resume|fresh}`), `queue[]` (`QueueItem {id, content, origin, sending, queued_at}`), `todos[]`, `auto_continue {status, reason?, runs_used, budget}`, `jobs[]`, `peers[]` (`Peer {peer, relation: task|parent|child|grant, task?}`).

**`Job`**: `{id, name, command, cwd, machine_id?, workspace_id?, status: running|succeeded|failed|timed_out|killed|lost, exit?, signal?, started_at, ended_at?, timeout_s?, log_path, lines, follow?}`. A job runs on its workspace's host; `cwd` and `log_path` (`<workspace root>/.controlroom/jobs/<id>.log`) are on that machine. The agent daemon always fills `machine_id` and `workspace_id`; `lost` means its supervisor died with the host before it exited.

**`Request`** (one card for all kinds): `{id, kind: permission|question|dialog|sandbox|contact|spawn|terminal_input|privileged, blocking, state: pending|answered|withdrawn|expired, title, detail?, body?, options[], questions[], answer?, origin_child?, created_at}`.
`body` is a structured subject: `command · path · endpoint · diff · git_push · contact · spawn · terminal_input {machine, shell, command} · privileged {machine, command, reason, cwd}`. Title, detail and options are always enough to answer, so unknown kinds stay answerable.
Answers are `Response`: `choice {option_id, reason?}` · `answers {answers[]}` · `dismiss` (questions: tells the agent "declined"). The recorded `answer` carries the deciding `DeviceRef` (`request.respond.by`, set by the controller).

- **Sandbox approvals** (spec §11.4) come from a workspace's sandbox on a host: nono asks the host daemon's loopback webhook, the host raises an approval (`approval.request`), and the agent daemon shows it as a blocking `sandbox` request on the workspace's session (state `awaiting_permission`, attention `approval`, a timeline `request` item). The user's choice goes back with `approval.respond`. Bodies: `git_push {command, remote, branch, force}` for a mediated `git push`, else `command`, `path {path, access}` or `endpoint {method, url}`. Options: `allow-once` / `deny` for commands and endpoints, `allow-session` / `deny` for file access. A `deny` choice's `reason` is passed to the agent. The first resolution wins; nono's timeout, nono hanging up or a host restart close it `expired` (`approval.closed`); expired and withdrawn approvals are denials.
- **`terminal_input`**: an agent command waits for terminal input (the host reports it after 2 s without output); the user types in the terminal pane, the agent can't. Withdrawn when the command moves on; unanswered for 10 min (setting) the command is interrupted and its result says so.
- **`privileged`** (`bash_privileged`, R3): every call needs approval; the command then runs unsandboxed on the host in the privileged pane.

**Attribution.** `DeviceRef {device_id, label}` (label at the time; UIs show the current label, falling back to this). `Author` (todos): `agent · user{device} · system`. `PromptOrigin`: `user{device} · schedule{schedule_id, name}`.
**Peers.** `PeerRef {daemon_id, session_id, name}`: with the agent daemon every session is local, so `daemon_id` is the agent daemon's own id (the mailbox rework in R3 is expected to drop it).

## 4. Timeline (`timeline`)

`TimelineItem {id, seq_from, seq_to, actor: "root" | child id, ts, streaming, kind, ...}`, ordered by `seq_from`.

| kind | fields |
| --- | --- |
| `run` | `trigger` (user · queue · steer · inject{origin, label} · auto_continue · retry · extension{name} · schedule{schedule_id, name}), `state` running/settled, `outcome?`, `error?`. Injection origins: user · job · child · message · answer · todo · continue · tool_late (a late tool result) · workspace (host unreachable past the grace, workspace added or removed, shell reset) |
| `message` | `role` user/agent/notice, `content[]` (text · image · resource) |
| `thinking` | `text` |
| `tool_call` | `tool_call_id, parent_tool_call_id?, name, title, tool_kind, input?` (display text), `status, output?` (text · diff · terminal), `exit?, duration_ms?`. Remote tools keep pi's names and argument shapes, so `read devbox:~/src/api/main.rs` maps like a local read |
| `todo` | `items[]` (snapshot after a change) |
| `request` | the `Request` fields inline |
| `subagent` | `child: PeerRef, title, agent, state, outcome?, usage?, result?` |
| `job` | `job: Job` (nested: `id, name, command, cwd, machine_id, workspace_id, status, exit?, signal?, started_at, ended_at?, timeout_s?, log_path, lines, follow?`); upserted when the job starts, changes its follow, and ends |
| `session_message` | `message_id, direction, peer, text, reply_to?, expects_reply, outcome?` (delivered · queued · connection_lost · denied · expired), `late` |
| `usage` | `scope, tokens {input, output, cache_read, cache_write}, cost?, context?` |
| `compaction` / `retry` | `reason, phase, tokens_before?, tokens_after?, error?` / `attempt, max_attempts?, phase, delay_ms?, error?` |
| `notice` / `error` / `session_info` | `level, text` / `text` / `title?, model?, thinking?` |

`TimelineOp`: `upsert {item}` (insert or replace whole) · `append_text {id, text, seq_from, seq_to}` (message's last text part, thinking text, or tool text output) · `end {id, seq_from, seq_to}` (clears `streaming`).
`RawFrame {seq, ts, stream: to_agent|from_agent|stderr|meta, text}` only with `raw_since`.

## 5. Agent daemon API (`agentd`)

Controller ↔ `controlroom-agentd`, session-scoped: sessions are global, the mailbox is local (no outbox or inbox), and hosts are reached through the controller's `hostops` bridge. The agent daemon never calls the controller.

Lifecycle: `hello` first (else `hello_required`), then subscriptions. Disconnecting never triggers anything. Every mutating command carries `op_id`; a repeat returns the original result, a reuse for a different command is `op_id_conflict`.
**One controller per daemon.** `hello.role` is `{kind: "controller", controller_id}` or `{kind: "debug"}` (`controlroom-agentd ctl`). A different `controller_id` while one is connected fails with `controller_conflict {controller_id, connected_since}`; the same one replaces its old connection. Debug clients connect alongside but get `unsupported` from `hostops.*`.

| Method | Purpose |
| --- | --- |
| `hello {protocol_version, client_name, role}` | → `daemon_version`, `protocol_version`, `daemon_id`, capabilities |
| `sessions.subscribe {since_rev}` | every summary + settings; incremental from a persisted rev log, else `snapshot {sessions[], settings}`; then `sessions.events {changes[{rev, kind: session_upserted\|session_removed\|settings_changed}]}` |
| `session.subscribe {id, since_seq, raw_since?, timeline?}` | backlog = upsert of each item changed after `since_seq`, then live ops; result carries `head_seq` and `detail`. `timeline: false`: detail only |
| `unsubscribe {subscription_id}` | end any subscription |
| `session.create {workspaces[{machine_id, path}], name?, parent_id?, sandbox_level?, sandbox?, prompt?}` | create a session on its workspaces (the first is primary) and start pi on the server; every workspace's machine must be connected (`machine_unavailable`). `prompt {content, origin?}` is queued in the same transaction under the same `op_id` (result `prompt: queued{queue_id}`) |
| `session.workspace_add {id, workspace{machine_id, path}}` → `Workspace` · `session.workspace_remove {id, workspace_id}` | add or drop a workspace; the agent learns of it through `session.info` and a notice. The primary can't be removed (`invalid_params`); a workspace with a running call or job is `busy` |
| `session.prompt {id, content, origin?, if_busy?}` | send, steer or queue → `sent · steered · queued{queue_id} · skipped` |
| `session.cancel` · `close` · `reopen` · `delete` | lifecycle (cancel also interrupts running tool calls; close stops the shells; delete removes the workspaces' state on the hosts) |
| `session.resume {id, sandbox?}` · `session.start_fresh {id, include_history, sandbox?}` | reattach / new agent session (→ `history_path?`) |
| `session.update {name?, auto_send?, auto_continue?, auto_continue_budget?}` | session settings |
| `session.mark_read {seq}` · `session.mark_unread` | shared read cursor → `{read_seq, unread}` |
| `session.set_option {option_id, value}` | model, thinking level, … |
| `queue.send_now {queue_id, interrupt?}` · `queue.edit` · `queue.remove` · `queue.reorder` · `queue.resume` | queue |
| `request.respond {request_id, response, then?, by?}` | answer any request kind (including host sandbox approvals and `terminal_input`); first answer wins (`already_resolved` carries the winner) |
| `todo.add {texts[], index?, by?}` · `todo.update` · `todo.remove` · `todo.reorder` | shared todo list |
| `job.list` · `job.logs {from_line?, max_lines?}` · `job.kill` · `job.follow {buffer_seconds?, pattern?, from?}` · `job.unfollow` | jobs on the workspaces' hosts (`job.logs` reads the log from its host) |
| `sandbox.revoke {grant_id}` | remove a session grant |
| `agents.list` · `doctor.report` · `settings.get` · `settings.set` | the server's agents and checks; settings (`idle_timeout_h`, retention) |
| `models.list` · `set_key {provider, key, by?}` · `set_gateway {provider, gateway{base_url, models[], auth_header}, key?, by?}` · `remove` · `sign_out` · `test` · `set_defaults {defaults{provider?, model?, thinking?}, by?}` → `ModelsState` | Settings › Models: providers managed here next to the server's config (read-only, wins on a clash; `ModelProvider {id, label, source: none·ui·server, takes_key, can_sign_in, key{last4, added_at, added_by}?, signed_in{account?}?, gateway?, test{ok, message, at}?, clash?}`), pi's models, the defaults of new sessions. A key is never returned. Changes apply to new sessions and restarts |
| `models.login_start {provider, by?}` · `login_input {login_id, input, by?}` · `login_wait {login_id, after}` · `login_cancel` → `LoginState {login_id, provider, seq, step}` | a subscription sign-in with pi's own flow on the server: `step` is `progress · choose{options[]} · open_url{url} · device_code{user_code, verification_uri} · input · done{account?} · failed{message} · cancelled · timed_out`; `input` answers a choice, or is the redirect URL the browser ended on; `login_wait` returns once `seq > after` (≤ 20 s). One sign-in at a time, 10 minutes at most |
| `hostops.subscribe` · `hostops.respond` · `hostops.notify` · `hostops.machines` | the host bridge (controller only), below |

Notifications: `sessions.events`, `session.events {head_seq, ops[], raw[], detail?}`, `hostops.request`, `hostops.withdraw`.

**Host bridge (`hostops.*`), pull-style.** The controller owns SSH keys, host trust, bootstrap and connectivity, so it carries every host request without deciding any:

1. After `hello` the controller calls `hostops.subscribe` → `{subscription_id, requests[]}`: **every** pending request (replacing what it held). New ones follow as `hostops.request {requests[]}`; requests the agent daemon no longer needs as `hostops.withdraw {ids[]}`.
2. `HostOpRequest {id: HostOpId, machine_id, session_id?, method, params}` (the host request's `method` and `params` sit next to the routing fields; never `hello`). The controller sends it when that machine is connected and answers with `hostops.respond {id, result | error}`, forwarding the host's response verbatim. A request whose host connection dropped before the response is sent again on the next connection: **every host method is idempotent**. An unknown machine is answered `not_found {resource: machine}`. Duplicate responses are ignored (first wins).
3. Every notification a host connection receives is forwarded as `hostops.notify {machine_id, notification: {method, params}}`.
4. `hostops.machines {machines[{machine_id, name, availability, epoch?, grace_s}]}` after `hello` and on every change: the names the agent uses in `machine:path` (unique, no `:`), availability, the host daemon's epoch (a change means it restarted) and the per-machine grace (default 600 s).

The agent daemon owns everything else: the pending calls and their grace, `journal.ack`, the per-host job cursors, re-subscribing (`jobs.subscribe {since}`, `approvals.subscribe`) after every new host connection, and turning host approvals and waits for input into session requests.

**Errors** (`ErrorData`, by `kind`, plus `code`/`message`): `parse_error · invalid_request · method_not_found · invalid_params · internal_error · incompatible_version · hello_required · op_id_conflict · timeout · invalid_response · unsupported · not_found{resource, id} · unknown_session · deleted · already_resolved · busy · invalid_state{state} · not_resumable · agent_unavailable · sandbox_unavailable · fs_error · controller_conflict{controller_id, connected_since} · machine_unavailable{machine_id, connectivity} · conflict{path} · lost{may_have_run} · shell_gone{shell} · connection_lost{machine_id, since} · unknown`. `not_found` resources add `workspace`, `call`, `approval`, `provider` and `sign_in`.

## 6. Host API (`host`)

Controller ↔ `controlroomd` host role, over the exec channel `controlroomd connect`. Same framing, tolerance and single-controller rules as §5. A host keeps no sessions: it knows **workspaces** (one session's directory with its own nono profile), **shells** (PTY supervisors), **jobs**, and a **journal** of tool calls keyed by `CallId`. Everything the agent triggers runs under the workspace's profile: shells, jobs, and the fs worker (`controlroomd workspace-exec`) that serves the file and search ops, so the unsandboxed host daemon never touches project files on the agent's behalf.

| Method | Purpose |
| --- | --- |
| `hello {protocol_version, client_name, role}` | → `host_version`, `protocol_version`, `daemon_id`, `epoch` (new on every daemon start), capabilities |
| `workspace.open {workspace_id, session_id, path, sandbox: SandboxSpec}` | resolve and check the directory, write the profile (`sandbox.level` is the session's effective level) → `{root, home, context_files[{path, content}], skills[{name, description, path}]}` (AGENTS.override.md / AGENTS.md / CLAUDE.md per directory from `/` down to the root). Idempotent by id; again with a changed sandbox rewrites the profile |
| `workspace.close {workspace_id}` | stop its shells and jobs; idempotent |
| `shell.open {workspace_id, name, cwd?}` → `HostShell` · `shell.close {shell_id}` · `shell.list {workspace_id?}` | named PTY shells (`HostShell` = `Shell` + `shell_id`); `open` is idempotent by name and reopens a `gone` shell in its last directory with `reset_pending` |
| `tool.run {call_id, workspace_id, op: HostOp}` | → `HostResult`. A repeat with the same `call_id` attaches to the running call or returns the journaled result; never reruns |
| `tool.attach {call_id}` | → the result of an earlier call (`not_found {resource: call}` if it never started; `lost` if the daemon restarted without its result) |
| `tool.cancel {call_id}` | → `{cancelled}`: ETX into the shell's PTY, SIGKILL of the foreground group after 5 s; the result follows with `cancelled` and the partial output |
| `journal.ack {call_ids[]}` | the agent daemon logged these results; the host may drop them (TTL 24 h otherwise) |
| `jobs.subscribe {since?}` → `{subscription_id, rev, events[]}` · `jobs.ack {rev}` | the job event log (below) |
| `approvals.subscribe` → `{subscription_id, pending[]}` · `approval.respond {approval_id, option_id, reason?}` | sandbox approvals of the host's workspaces (controller only) |
| `unsubscribe {subscription_id}` | end a subscription |
| `fs.list {path, include_files?}` · `doctor.report` | the new-session dialog and file browser (the host user's own access, unsandboxed); nono, Landlock, persistence and boot-start checks |

Notifications: `tool.output {call_id, text}` (ANSI-stripped, coalesced; to every connection waiting on the call), `tool.waiting_input {call_id, waiting}`, `shell.changed {shell: HostShell}`, `jobs.event {subscription_id, events[]}`, `approval.request {subscription_id, approval}`, `approval.closed {subscription_id, approval_id, state: expired|withdrawn}`.

**`HostOp`** (tagged by `op`, `unknown` fallback). Paths are `RemotePath`s: absolute, `~`, or relative to the workspace root.

| op | params | result (`HostResult`, same `op` tag) |
| --- | --- | --- |
| `read` | `path, offset?` (line, from 1), `limit?` | `{path, size, content: text{lines[], start_line, total_lines, cut?: lines\|bytes, first_line_too_long} \| image{mime_type}}`: pi's limits (2000 lines, 50 KB) |
| `read_bytes` | `path, offset?, length?` | `{path, data (base64), length, size, sha256?}` (≤ 20 MiB per call; `sha256` of the whole file when read whole) |
| `stat` | `path` | `{path, exists, kind?, size?, mtime?}` |
| `write` | `path, content, encoding?: utf8\|base64, expect_sha256?` | `{path, bytes, sha256}`; temp file + fsync + rename, parent dirs created; `conflict{path}` when `expect_sha256` doesn't match |
| `ls` | `path?, limit?` (500) | `{path, entries[FsEntry], total}` |
| `find` | `pattern` (glob), `path?, limit?` (1000) | `{path, paths[] (relative), limit_reached}` |
| `grep` | `pattern, path?, glob?, ignore_case?, literal?, context?, limit?` (100) | `{path, lines[{path, line, text, context}], matches, limit_reached, lines_truncated}` |
| `bash` | `shell` (its name), `command, timeout_s?` (600, enforced on the host) | `{exit_code?, signal?, cwd, output (last 2000 lines / 50 KB), truncated, log_path, timed_out, cancelled, waited_for_input, shell_reset?, duration_ms}`; a non-zero exit is a result |
| `job_run` | `command, cwd?, name?, timeout_s?, follow?{pattern?, buffer_seconds}` | `{job: Job}` |
| `job_follow` | `job_id, pattern?, buffer_seconds?, from?` | `{job}` |
| `job_unfollow` · `job_kill` | `job_id` · `job_id, by: agent\|user` | `{job}` |
| `job_status` | `job_id?` | `{jobs[]}` |

**Exactly once.** Reads (`read`, `read_bytes`, `stat`, `ls`, `find`, `grep`) are idempotent: a resend within the journal's lifetime returns the journaled result; after a daemon restart they rerun. `write` is journaled: a resend returns the journaled result; if the daemon died mid-write the file's hash tells (the new content → the result; otherwise `lost {may_have_run: false}`, safe to retry). `bash` is at-most-once: the journal row is fsynced before the command reaches the shell; a resend attaches; after a daemon restart the PTY supervisor's log gives the result, without it the call is `lost {may_have_run: true}`. `job_*` are at-most-once by call id (a resend returns the same job). An `edit` is the extension's: `read_bytes` (`<id>/read`) then `write` with `expect_sha256` (`<id>/write`), each step resent by its own id; a changed file is a `conflict`, never a lost update.

**Job event log.** The host follows every running job and appends `JobEvent {rev, at, job_id, kind, …}` to a persisted log with one revision sequence per host: `started {session_id, workspace_id, name, command, cwd, log_path, timeout_s?, follow?}` (logged before the process exists) · `progress {lines, offset, matches[]}` · `follow {pattern?, buffer_seconds, from, matches[]}` · `unfollow` · `killed {by}` · `exit {exit_code?, signal?, timed_out, lost?, lines, duration_ms}` (once the output is fully scanned; `lost: "host rebooted"` when the supervisor is gone). The host never decides wakes. After every `hello` the agent daemon subscribes from its cursor, gets the backlog, then live `jobs.event`; it commits the new cursor, the pending follow matches and the wakes they produce in one transaction (wake ids `exit:<machine>/<job>` and `follow:<machine>/<job>:<first>-<last>`), so each wake is delivered exactly once, and acks with `jobs.ack {rev}` so the host can prune ended jobs' events (design §3.6).

**Terminals** (`host::terminal`). Each attached terminal is its own exec channel: `controlroomd pty attach <shell id> --framed [--since-seq <seq> --instance <instance>]`. Frames both ways are `kind (u8) · length (u32 BE) · payload`; unknown kinds are skipped. To the host: `input` (bytes typed in; any client may type) and `resize {cols, rows}`. From the host: `attached {instance, from_seq}` first, then `output {seq, bytes}` per transcript record, and `foreground` (the foreground program's name, empty at the prompt) when it changes. Without `--since-seq` the last 1000 records are replayed; a resume continues at `seq` while the shell still runs as `instance` and is at most 20 000 records behind, else it replays afresh (the client clears its screen). The channel ends with exit 0 when the shell's process ends; non-zero means nothing to attach to. Without `--framed`, `pty attach` speaks raw bytes for a human.

## 7. UI API (`ui`)

Lifecycle: `hello {device_id}`, `devices.register`, `state.subscribe`, `session.subscribe` per open session (shared across tabs by the server, resumed without duplicates). `op_id`s pass through unchanged.

| Method | Purpose |
| --- | --- |
| `hello` · `ping` | → server version, protocol, `auth` (oidc · dev_token), `user`, `push {vapid_public_key}?`, capabilities · heartbeat |
| `state.subscribe {since: {epoch, rev}?}` | aggregated feed (below) |
| `session.subscribe {id, since_seq, raw_since?, timeline?}` · `unsubscribe` | as in the agent daemon API |
| `session.create {workspaces[], name?, parent_id?, sandbox_level?, prompt?, task_ids?}` | the agent daemon's create (the controller fills `sandbox` from the workspaces' machines and the tasks' projects, and the prompt `origin`), then links the tasks (moving `backlog`/`todo` tasks to `doing`); idempotent under one `op_id` |
| *pass-through* | `session.*` (incl. `workspace_add`, `workspace_remove`), `queue.*`, `request.respond`, `todo.*`, `job.*`, `sandbox.revoke`, `agents.list`, `settings.get`, `settings.set`, `models.*`: the agent daemon's params unchanged, no `machine_id` (the controller fills `by` with the connection's device and logs `models.*` changes with it, never their params). The agent daemon unreachable → `timeout` |
| `fs.list {machine_id, path, include_files?}` · `doctor.report {machine_id?}` | a host's listing; a host's or (without `machine_id`) the server's checks |
| `machines.add` → `add_confirm` · `confirm_host_key` · `remove` · `reconnect` · `upgrade` · `set_settings {name?, key?, auto_upgrade?, grace_s?}` | machines (`remove` leaves sessions; their workspaces there become unavailable). Names are unique and contain no `:`, `/`, whitespace or control characters (agents write `machine:path`); `grace_s` (default 600, at most a week) is the tool-call grace reported to the agent daemon |
| `keys.list` · `rotate` · `generate` · `import` · `delete` | SSH keys |
| `projects.create {name, color}` · `update {project_id, name?, color?}` · `delete` | projects are named, colored groups of tasks |
| `tasks.create` · `update` · `move` · `delete` · `link {task_id, session_id}` · `unlink` · `checklist_add` · `checklist_update` · `checklist_remove` | tasks (`project_ids`, `links: [session_id]`, checklist) |
| `grants.revoke` | contact grants |
| `devices.register` · `rename` · `remove` | devices (remove signs the device out) |
| `notifications.preferences` · `set_preferences` · `subscribe` · `unsubscribe` | Web Push |
| `schedules.create {session_id, …}` · `update` · `delete` · `run_now` | schedules (§8.1, capability `schedules`): `Schedule {session_id, spec: cron{expr}·interval{every_s ≥ 60}, timezone, message, enabled, if_busy, missed, next_run, pending_run, recent_runs[]}`; fired as `session.prompt` with `origin: schedule` and `op_id = <schedule id>:<due ms>` |
| `sandbox.profiles` · `sandbox.set_profile {scope, change}` | sandbox defaults (§11.6) |
| `client.focus {device_id, session_id?, visible}` | push deduplication |

**State feed.** `StateSnapshot {machines[], sessions[FeedSession], overlaps[], projects[], tasks[], grants[], schedules[], devices[]}`; `state.events {changes[{rev, kind}]}` with `machine_* · session_upserted{session} · session_removed{session_id} · overlaps_changed{overlaps} · project_* · task_* · grant_* · schedule_* · device_*` (`_upserted` / `_removed`).
`FeedSession` = the agent daemon's `SessionSummary` + `project_ids[]` (the projects of the tasks that link it; empty = **Unassigned**; the controller computes it). `Overlap {machine_id, path, session_ids[]}`: a directory used by several open sessions' workspaces (allowed; the UI warns). `ContactGrant {id, from: session_id, to: session_id, scope, granted_at, by?}`.
Notifications: `state.events`, `session.events`.

**Connectivity** (`Machine.connectivity`, by `state`): `connected · connecting · unreachable · incompatible · action_required {reason: host_key_changed|auth_failed|host_key_unconfirmed|controller_conflict, presented_host_key?}`, plus `Machine.update_available`. The agent daemon's settings (idle stop, retention) are the server's (`settings.get` / `settings.set`), not a machine's.
UI-only errors: `unauthenticated · machine_unavailable{machine_id, connectivity} · host_key_mismatch · unsupported_platform · key_in_use · invalid_key`.

**Terminal streams** (`terminal`). `GET /api/stream/terminal/<session id>/<shell name>` upgrades to a WebSocket under `/api/ws`'s Origin, Host and login rules (an unknown session or shell is a 404). Binary frames carry terminal bytes both ways; text frames carry `TerminalCommand` (`resize {cols, rows}`; the last resize from any browser wins) and `TerminalEvent` (`attached {reset}` · `detached {reason}` · `foreground {name?}` · `ended {reason}`). The controller attaches over the machine's SSH connection (one exec channel per stream), keeps the socket through a link drop (`detached`, then `attached {reset: false}` where the output left off), and catches a slow browser up from the transcript instead of stalling the connection. Close codes: 1000 after `ended`, 4401 when the login is gone (logout, device or login revoked), 1001 at shutdown, 1008 for an invalid frame.

## 8. Agent API (`agent`)

The extension inside a session's pi talks to the agent daemon over that session's own unix socket, `<state>/run/<id>.agent.sock`. The session's server-side sandbox profile grants only that socket (no project files, no SSH, no host network), and a socket serves only its own session.
The launch env file sets `CONTROLROOM_SESSION_ID`, `CONTROLROOM_AGENT_SOCKET` and `CONTROLROOM_AGENT_TOKEN` (a fresh token per launch). Without them the extension registers nothing.

Lifecycle: `hello {protocol_version, token, client_name}` first (`hello_required` otherwise; a wrong token is `unauthenticated`, a major mismatch `incompatible_version`); then the daemon sends `state` and, with `remote_tools`, `session.info`, and again on every change.
Mutating calls carry `call_id` (pi's tool call id) as their idempotency key: a call resent after a reconnect applies once.

| Method | Purpose |
| --- | --- |
| `hello {protocol_version, token, client_name}` | → `daemon_version`, `protocol_version`, `session_id`, `capabilities` |
| `question.ask {call_id, title?, questions[]}` · `question.withdraw {call_id, question_id}` | non-blocking questions (`ask_user`) |
| `todo.list` · `todo.add {call_id, texts[]}` · `todo.update {call_id, todo_id, text?, status?, reason?}` | → `{todos[]}`, attributed to the agent |
| `notify {call_id, text, level?}` | a `notice` item in the timeline |
| `job.run {call_id, command, machine?, cwd?, timeout_s?, name?}` | → `{job_id, log_path}`: on `machine`'s workspace (default the primary), under its sandbox; `cwd` relative to that workspace root |
| `job.follow {call_id, job_id, buffer_seconds?, pattern?, from?}` · `job.unfollow` · `job.status {job_id?}` · `job.kill` | jobs; the agent's own kill sends no exit wake |
| `session.info` | → `SessionInfo {workspaces[WorkspaceInfo], shells[AgentShell]}` (`remote_tools`) |
| `host.call {call_id, machine, op: HostOp}` | → `{result: HostResult, link_drops}` (`remote_tools`) |
| `host.cancel {call_id}` | the tool's abort signal |
| `shell.open {call_id, name, machine, cwd?}` → `AgentShell` · `shell.list` → `{shells[]}` · `shell.close {call_id, name}` | the session's named shells (`shell_new`, `shell_list`) |

Notifications: `state {todos[], questions[], auto_continue}`, `session.info` (`SessionInfo`), `host.output {call_id, text}`, `host.waiting_input {call_id, waiting}`.

**Remote tools.** pi runs with `--no-builtin-tools --no-context-files --no-skills`; the extension registers `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls` (pi's names and argument shapes, paths as `machine:path`) plus `shell_new`, `shell_list`, and builds the system prompt's project-context, `<workspaces>` and skills sections from `session.info`.

- `WorkspaceInfo {machine, path, root?, home?, primary, shell, availability, context_files[], skills[]}`: `machine` is the display name the agent uses; `shell` the workspace's default shell; instruction files per workspace, outermost first (files shared by several workspaces are shown once). `AgentShell` = `Shell` + `machine`.
- `host.call`: the agent daemon checks `machine` is one of the session's workspaces (`not_found {resource: workspace}` otherwise), resolves bare and relative paths against that machine's (primary) workspace root, maps `bash.shell` to the host's shell, and routes the call. `job_*` ops are refused (`invalid_params`; jobs go through `job.*`). The `call_id` is pi's tool call id or `<id>/<n>` for the steps of one tool, so a resend after an extension reconnect is deduplicated end to end.
- **Link loss pauses, it doesn't fail.** A call whose host is unreachable stays open and is resent after the reconnect; the result's `link_drops` says how often the link dropped (the call still ran once). Past the machine's grace (default 10 min) the call fails with `connection_lost {machine_id, since}`; the agent daemon keeps polling `tool.attach` and injects the late result when the host returns (injection origin `tool_late`).
- Cancellation: pi's abort → `host.cancel` → `tool.cancel` → ETX in the PTY; the shell survives, the result is the partial output with `cancelled`.

Daemon → extension traffic goes through this socket rather than `/controlroom <verb>` commands on pi's stdin: those would be transcript lines that need a ready agent, follow the in-doubt rules, and make pi wait for the handler. Answers, todo notices, continuations and wakes reach the model through the agent's own RPC (injection: `steer` while it works, a `prompt` while it is idle), never through the socket.

**Runtime rules behind it** (spec §8): an injection carries its origin, which becomes the run's trigger (`inject {origin, label}`; a continuation is `auto_continue`). Auto-continue (on by default, budget 10 runs per user instruction) starts a continue run when the agent ends its turn with pending or in-progress todos, unless a question or an approval is open, background work is running (jobs, children, asks, host calls), the run failed or was cancelled, or the queue (or another injection) has items. It pauses with attention when only blocked items remain (`paused`), after 3 continue runs that closed no item (`stalled`), when the budget is used (`budget`), and on a user cancel (`paused`).

**Job wakes** (spec §8): a job's exit always wakes the agent (unless the agent killed it) with the job id, exit code or signal, duration, line count and log path (`machine:path`); a followed job also wakes it with the new or matching line ranges once its buffer (measured in host time) elapses. Wakes due together are merged into one injection with origin `job`. Log content never enters a wake. Wakes are computed from the host's job event log and recorded with the cursor in one transaction, so each is delivered exactly once across link loss and restarts. While a host is unreachable nothing is invented: `job.status` reports the last known state, running jobs still count as live work, and one notice per outage is injected after the grace.

## 9. TypeScript

Two sets, each with one file per type, a method map, `versions.ts` and `index.ts`:

- `web/src/lib/protocol/`: the UI API (`UiMethods.ts`);
- `extensions/pi/src/protocol/`: the agent API (`AgentMethods.ts`).

The host and agent daemon APIs have no TypeScript consumer, so no set. Regenerate both: `CONTROLROOM_UPDATE_TS=1 cargo test -p controlroom-protocol --test typescript`. The `protocol-ts-drift` flake check fails when the checked-in files differ; `web-check` type-checks the web set.

## 10. Transition (done)

The R1/R2 lanes removed the per-machine model:

- the daemon API (`daemon`, `DAEMON_PROTOCOL_VERSION`): `machine.subscribe` and its feed, the cwd-based `session.create`, `fs.list` on the daemon, and the mailbox (`outbox.*`, `inbox.deliver`). The session-scoped command types, `hello` and the settings live in `agentd`; `fs.list` and `doctor.report` in `host`;
- the per-machine `SessionSummary` (`cwd`, `agent`, `spawned_by`): `session::SessionSummary` is the global one;
- the old UI method table with `OnMachine` pass-through, `MachineSession` (`stale`), `SessionLink`, projects with location prefixes (`LocationPrefix`), machine-scoped tasks, schedules, grants and focus, and `Machine.settings`: `ui::next` became `ui` (its TypeScript `web/src/lib/protocol/`). `OnMachine` stays for `fs.list`.

`Job.machine_id` / `workspace_id` stay optional on the wire (a host's `job_*` results can't fill them); the agent daemon always fills them.

## 11. Open questions (judgment calls to review)

1. **Unknown fallbacks drop the raw tag.** An older controller re-encodes a newer host's `"hibernating"` state as `"unknown"`. Keeping raw tags would make TS unions non-exhaustive (`string`). OK?
2. **Summary vs. detail split.** Queue items, todos, auto-continue details, agent sessions and jobs are in `SessionDetail` (open session only); the summary keeps counts and, now, the workspaces and shells (the session header and terminal tabs need them for every session in view).
3. **`session.subscribe` resume** sends an `upsert` of every item changed after `since_seq` instead of replaying ops. There is no paging for very long timelines yet (could be added as `timeline.page`).
4. **Raw frames** use `raw_since: Seq` instead of `raw: bool`, so a seq link can open the raw view at a position.
5. **Tool input** is display text (`input?: string`), not structured JSON, to keep agent payloads off the wire.
6. **Auto-continue budget** is a run count; dollar budgets (§23) come later.
7. **Daemon-raised requests and job items in the timeline** have no transcript record: they sit at the transcript head when raised (`seq_from = seq_to = head`), and each change counts as changed after the head, so a client resuming with `since_seq = head` still receives it.
8. **Sandbox approval options.** File access offers "Allow for this session" (nono remembers a granted path); commands and endpoints "Allow once" only.
9. **Agent API transport.** A per-session socket plus a per-launch token: an agent can't even name another session.
10. **Todo notices don't wake the agent**; answers, wakes and continuations do. `notify` only adds a timeline notice.
11. **Host routing is generic.** The design note sketched `request.raise` for host-raised requests and separate `hostops.output` / `hostops.waiting_input` methods. Instead the controller forwards every host notification with `hostops.notify` and the agent daemon raises the requests itself (it already maps workspaces to sessions), so the controller stays a router that understands no host method. Machine names, epochs and the grace come with `hostops.machines` (the design's `hostops.connectivity`).
12. **`session.info` is its own notification** rather than part of `state`: it changes independently and carries instruction files, which shouldn't be resent on every todo change. It groups context files and skills per workspace (the design listed them flat with a `machine` field); the extension dedups shared parent files.
13. **One `HostOp` for both APIs.** `bash.shell` is the shell's name in both; the host resolves it in the call's session (`<session>/<name>`), so the agent daemon only rewrites the machine into a workspace. Job ops share the enum but `host.call` refuses them.
14. **Grace per machine** (`HostMachine.grace_s`) is the machine setting `Machine.grace_s` (`machines.set_settings {grace_s}`, default 600 s).
15. **Mailbox and peers (R3).** The agent daemon API has no outbox or inbox. How the controller's tasks and contact grants reach the agent daemon's contact policy (a pushed peer directory, or grants moving to the agent daemon) is left to the mailbox lane; `PeerRef` keeps its `daemon_id` until then.
16. **Not yet in the contract:** `bash_privileged` and `terminal_read` host ops (R3: the request kind and body exist), the `@machine:path` composer resolution, host-side diagnostics (pi-lens), and job secrets on hosts.
17. **Remote open questions of the design** are resolved in `docs/design/remote-tools.md` §9.
