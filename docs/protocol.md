# controlroom protocol 1.0

The contract between the browser, the controller (`controlroom-web`) and the daemon (`controlroomd`).
Source of truth: `crates/protocol` (Rust). TypeScript is generated into `web/src/lib/protocol/` (read-only for the UI).
Spec: `docs/architecture.html` §5, §7, §8, §8.1, §10, §11.4, §11.6, §12, §13.

Two APIs, both JSON-RPC 2.0 with newline-delimited framing (one compact JSON object per line, no batches):

| API | Between | Transport | Version constant |
| --- | --- | --- | --- |
| Daemon API | controller ↔ `controlroomd` | SSH exec channel `controlroomd connect` | `DAEMON_PROTOCOL_VERSION` = 1.0 |
| UI API | browser ↔ controller | one WebSocket per tab | `UI_PROTOCOL_VERSION` = 1.0 |

Rust areas (crate root modules): `ids`, `jsonrpc`, `error`, `version`, `session` (model), `timeline`, `daemon`, `ui`, `typescript`.

## 1. Conventions

- Field and enum names are `snake_case`. Tagged unions use `kind` (or `op` for timeline ops, `outcome` for results, `state` for connectivity).
- **Ids.** Store-assigned ids are validated ULID strings: `SessionId`, `DaemonId`, `QueueId`, `TodoId`, `JobId`, `GrantId`, `MessageId`, `OutboxId`, `SubscriptionId`, `ControllerId`, `MachineId`, `KeyId`, `ProjectId`, `TaskId`, `ChecklistItemId`, `ScheduleId`, `Epoch`.
  Opaque string ids: `OpId` (client idempotency key; schedules use `<schedule id>:<scheduled ms>`), `DeviceId` (browser-generated), `ItemId` and `RequestId` (assigned deterministically by the daemon's transcript fold, so a re-fold gives the same ids).
- `RemotePath` is a POSIX string (may start with `~`, expanded only by the daemon). `UnixMillis`, `Seq` (transcript position, from 1) and `Rev` (feed revision, from 1) are numbers.
- Optional **params** fields may be omitted. Optional **result** fields are always present, as `null` when empty.
- No agent-specific payloads anywhere. Drivers translate pi's RPC into these types. Raw transcript lines appear only as opaque text in `RawFrame`, and only on request.

## 2. Versioning and tolerance

**Versioning.** `major.minor`. Peers are compatible iff the majors match; a mismatch is the `incompatible_version` error (the machine shows `incompatible`).
A minor version only adds things an older peer can ignore or avoid:
new methods and notifications, new optional params fields, new result fields, new enum variants (including error kinds, timeline item kinds, request kinds).
Everything else is a major change.

**Capabilities.** `hello.capabilities` lists optional features as free strings; unknown ones are kept.
Daemon: `jobs` (`job.*`), `todos` (`todo.*`), `subagents`, `mailbox` (`outbox.*`, `inbox.deliver`), `sandbox_approvals` (the approval webhook runs: sandbox approvals reach the user as `sandbox` requests; without it, commands that need an approval are denied). Controller: `schedules`.
Today's daemon advertises `sandbox_approvals` (when its webhook listener is up) and nothing else.
A client calls a feature's methods only when the peer advertised it.
**Terminals (§22) and files (§23) are deliberately not designed yet.** They will arrive as later minor versions behind `terminals` / `files` capabilities, new methods, and new enum variants (e.g. a `privileged` request kind, a `terminal` request body), which 1.0 peers already decode as `unknown`.

**Tolerant decoding** (the prototype's "unknown variant hangs the call forever" bug, spec §16):

1. Every enum a peer *receives* has an `unknown` fallback (`#[serde(other)]`). A newer peer's variant decodes as `unknown` instead of failing the message.
   The raw tag is not kept (exceptions: capabilities are plain strings, and an unknown error keeps its `code` and `message`).
   Params-only enums (`FollowFrom`, `OptionValue`, `ProfileChange`) have none: an unknown value there is an `invalid_params` error for that call.
2. Unknown fields are ignored everywhere.
3. `Message::parse` classifies a line by shape and keeps params and results raw. A response whose `result` doesn't parse fails **only that call** (`RpcResponse::into_typed::<M>()` → `invalid_response`).
   A malformed `error` member still resolves its id (`RpcError::from_value_lenient`), and malformed error `data` becomes `unknown`.
   A line that isn't a usable message returns `MessageError { id, error }` with the id when readable, so the client can fail that one call.
   Notifications that don't decode are dropped and logged; nothing closes the connection.
4. **Timeouts are client-side.** Every call has one: 30 s by default, longer for known slow methods (`machines.add_confirm`, `machines.upgrade`, `doctor.report`, `fs.list`).
   On expiry the client fails the call with `timeout` and drops a late response. The controller returns `timeout` to the UI when its daemon call timed out.

## 3. Session model (`session`)

**`SessionSummary`** (every session, fed by `machine.subscribe`, sent whole on each change; preview throttled to 1/s):
`id, name?, title?, cwd, agent, created_at, updated_at, closed_at?, parent_id?, spawned_by?, state, last_outcome?, last_error?, read_seq, last_turn_end_seq, unread, preview, attention?, rollup, context?, queue, requests[], todo?, live_work, options[], agent_live, sandbox, resumable`.

- `state`: `starting · idle · working · awaiting_permission · awaiting_input · failed · dormant · interrupted · closed`.
- `last_outcome` (`RunOutcome`): `completed · cancelled · failed · max_tokens · refusal · unknown`.
- `attention` (server-owned): `approval` (blocking request) › `question` (lighter tier) › `problem` (failed, interrupted, auto-continue paused) › `unread`. `rollup` counts the same over same-machine descendants.
- `queue` (`QueueStatus`): `len, paused, auto_send, send_now_pending`. The items are in the detail.
- `requests[]`: pending requests, including descendants' (`origin_child`).
- `todo`: `{done, total (excl. skipped), auto_continue: on|off|paused|stalled|budget}`. `live_work`: `{jobs, children, asks}`.
- `options[]`: neutral agent settings (`SessionOption {id, label, kind: select{value, choices} | bool{value}}`), e.g. model and thinking level.
- `agent_live`: a live, ready agent process is attached, so options can be changed and the agent's requests answered. UIs disable option pickers while it is `false` (choices arrive with the agent's first state report).
- `sandbox`: `{level: strict|relaxed|off, default_level, explicit, looser_than_default, grants[]}`. `level` is the explicit level when one was chosen, else the default; `looser_than_default` drives the badge (`off` gets the warning badge). `context`: `{used, size?, cost?}`.

**`SessionDetail`** (one open session, from `session.subscribe`, replaced whole on change): `agent_sessions[]` (`{driver: pi|fake, native_id, started_at, ended_at?, started_by: new|resume|fresh}`), `queue[]` (`QueueItem {id, content, origin, sending, queued_at}`), `todos[]`, `auto_continue {status, reason?, runs_used, budget}`, `jobs[]`, `peers[]` (`Peer {peer, relation: task|parent|child|grant, task?}`: whom the session may contact).

**`Request`** (one card for all kinds): `{id, kind: permission|question|dialog|sandbox|contact|spawn, blocking, state: pending|answered|withdrawn|expired, title, detail?, body?, options[], questions[], answer?, origin_child?, created_at}`.
`body` is a structured subject: `command · path · endpoint · diff · git_push · contact · spawn`. Title, detail and options are always enough to answer, so unknown kinds stay answerable.
Answers are `Response`: `choice {option_id, reason?}` · `answers {answers[]}` · `dismiss` (questions: tells the agent "declined"). The recorded `answer` carries the deciding `DeviceRef` (`request.respond.by`, set by the controller).

**Sandbox approvals** (spec §11.4) are raised by the daemon itself, not the agent: nono asks the daemon's loopback webhook, and the daemon shows a blocking `kind: sandbox` request (state `awaiting_permission`, attention `approval`, a timeline `request` item).

- Bodies: `git_push {command, remote, branch, force}` for a mediated `git push`, else `command`, `path {path, access}` (file access) or `endpoint {method, url}`.
- Options: `allow-once` / `deny` for commands, endpoints and network destinations (nono asks per invocation); `allow-session` / `deny` for file access (nono remembers a granted path for the rest of its session).
- A `deny` choice's `reason` is passed to the agent. The first resolution wins: answered, `withdrawn` (the run was cancelled or the session closed) or `expired` (nono's timeout came first, nono hung up, or the daemon restarted while it waited). Expired and withdrawn approvals are denials.

**Attribution.** `DeviceRef {device_id, label}` (label at the time; UIs show the current label, falling back to this). `Author` (todos): `agent · user{device} · system`. `PromptOrigin`: `user{device} · schedule{schedule_id, name}`.
**Cross-machine references.** Daemon-side types name other sessions with `PeerRef {daemon_id, session_id, name}`; the UI maps `daemon_id` to a machine through `Machine.daemon_id`.

## 4. Timeline (`timeline`)

`TimelineItem {id, seq_from, seq_to, actor: "root" | child id, ts, streaming, kind, ...}`, ordered by `seq_from`.

| kind | fields |
| --- | --- |
| `run` | `trigger` (user · queue · steer · inject{origin, label} · auto_continue · retry · extension{name} · schedule{schedule_id, name}), `state` running/settled, `outcome?`, `error?` |
| `message` | `role` user/agent/notice, `content[]` (text · image · resource) |
| `thinking` | `text` |
| `tool_call` | `tool_call_id, parent_tool_call_id?, name, title, tool_kind, input?` (display text), `status, output?` (text · diff · terminal), `exit?, duration_ms?` |
| `todo` | `items[]` (snapshot after a change) |
| `request` | the `Request` fields inline |
| `subagent` | `child: PeerRef, title, agent, state, outcome?, usage?, result?` |
| `job` | the `Job` fields inline (`id, name, command, cwd, status, exit?, signal?, started_at, ended_at?, timeout_s?, log_path, lines, follow?`) |
| `session_message` | `message_id, direction, peer, text, reply_to?, expects_reply, outcome?` (delivered · queued · connection_lost · denied · expired), `late` |
| `usage` | `scope, tokens {input, output, cache_read, cache_write}, cost?, context?` |
| `compaction` / `retry` | `reason, phase, tokens_before?, tokens_after?, error?` / `attempt, max_attempts?, phase, delay_ms?, error?` |
| `notice` / `error` / `session_info` | `level, text` / `text` / `title?, model?, thinking?` |

`TimelineOp`: `upsert {item}` (insert or replace whole) · `append_text {id, text, seq_from, seq_to}` (message's last text part, thinking text, or tool text output) · `end {id, seq_from, seq_to}` (clears `streaming`).
`RawFrame {seq, ts, stream: to_agent|from_agent|stderr|meta, text}` only with `raw_since`.

## 5. Daemon API (`daemon`)

Lifecycle: `hello` first (else `hello_required`), then subscriptions. Disconnecting never triggers anything. Every mutating command carries `op_id`; a repeat returns the original result, a reuse for a different command is `op_id_conflict`.

**One controller per daemon.** `hello.role` is `{kind: "controller", controller_id}` or `{kind: "debug"}`.

- A daemon serves exactly one controller at a time. A controller `hello` with a *different* `controller_id` while one is connected fails with `controller_conflict {controller_id, connected_since}`.
- A controller `hello` with the *same* `controller_id` replaces the old connection: the daemon closes it (ending its subscriptions), so stale or half-open connections never block a reconnect.
- Debug clients (`controlroomd ctl`) don't count as the controller. Any number may connect alongside it; they may subscribe and issue every command, but `outbox.*` and `inbox.deliver` answer them with `unsupported`.
- The connected controller is the mailbox broker. Unacknowledged outbox items survive disconnects; after a reconnect the controller resubscribes with the last rev it saw.

| Method | Purpose |
| --- | --- |
| `hello {protocol_version, client_name, role}` | → `daemon_version`, `protocol_version`, `daemon_id`, capabilities |
| `machine.subscribe {since_rev}` | every summary + settings; incremental from a persisted rev log, else `snapshot` |
| `session.subscribe {id, since_seq, raw_since?, timeline?}` | backlog = upsert of each item changed after `since_seq` (from the materialized timeline), then live ops; result carries `head_seq` and `detail`. `timeline: false` (default `true`): detail only, no backlog and no ops |
| `unsubscribe {subscription_id}` | end any subscription |
| `session.create {agent, cwd, name?, parent_id?, spawned_by?, sandbox_level?, sandbox?, prompt?}` | create and start a session (`sandbox_level`: explicit user choice; `sandbox`: the controller-computed defaults). `prompt {content, origin?}` is queued in the same transaction under the same `op_id` and sent once the agent is ready (result `prompt: queued{queue_id}`) |
| `session.prompt {id, content, origin?, if_busy?}` | send, steer or queue → `sent · steered · queued{queue_id} · skipped` |
| `session.cancel` · `close` · `reopen` · `delete` | lifecycle |
| `session.resume {id, sandbox?}` · `session.start_fresh {id, include_history, sandbox?}` | reattach / new agent session (→ `history_path?`); `sandbox` refreshes the defaults, an explicit level stays |
| `session.update {name?, auto_send?, auto_continue?, auto_continue_budget?}` | session settings |
| `session.mark_read {seq}` · `session.mark_unread` | shared read cursor → `{read_seq, unread}` |
| `session.set_option {option_id, value}` | model, thinking level, … (`not_found {resource: option}` for an unknown option) |
| `queue.send_now {queue_id, interrupt?}` (→ `sent · steered · interrupting · queued`) · `queue.edit` · `queue.remove` · `queue.reorder` · `queue.resume` | queue. Send-now steers, or cancels at a safe point (no tool running); `interrupt: true` cancels at once and sends the item as the next prompt; `queued` while the agent isn't ready |
| `request.respond {request_id, response, then?, by?}` | answer any request kind; first answer wins (`already_resolved` carries the winner). `by` is the deciding device, filled in by the controller (it overwrites a client's value) |
| `todo.add {texts[], index?}` · `todo.update` · `todo.remove` · `todo.reorder` | shared todo list (`todos`) |
| `job.list` · `job.logs {from_line?, max_lines?}` · `job.kill` · `job.follow {buffer_seconds?, pattern?, from?}` · `job.unfollow` | jobs (`jobs`) |
| `sandbox.revoke {grant_id}` | remove a session grant ("Remove · restarts agent") |
| `agents.list` · `fs.list {path, include_files?}` · `doctor.report` · `settings.get` · `settings.set` | machine queries and settings |
| `outbox.subscribe {since}` · `outbox.ack {item, outcome}` · `inbox.deliver {op_id, id, delivery}` | durable mailbox, controller only (`mailbox`) |

Notifications: `machine.events {changes[{rev, kind: session_upserted|session_removed|settings_changed}]}`, `session.events {head_seq, ops[], raw[], detail?}`, `outbox.items {items[]}`.

**Mailbox.** The daemon never calls the controller. Outbox items: `message` · `spawn` · `resolved` (the user's answer to a controller-raised request).
`outbox.ack` outcomes: `delivered · spawned{child} · queued{expires_at}` (interim) `· denied · connection_lost{scope: controller|peer, since} · expired`.
Inbox deliveries: `message` (incl. replies, `late`) · `request` (contact or spawn approval to show on the session) · `peers` (the session's contact directory, shown as `SessionDetail.peers`) · `child_update` (remote child state).

**Errors** (`ErrorData`, by `kind`, plus `code`/`message`): `parse_error · invalid_request · method_not_found · invalid_params · internal_error · incompatible_version · hello_required · op_id_conflict · timeout · invalid_response · unsupported · not_found{resource, id} · unknown_session · deleted · already_resolved · busy · invalid_state{state} · not_resumable · agent_unavailable · sandbox_unavailable · fs_error · controller_conflict{controller_id, connected_since} · unknown`.

## 6. UI API (`ui`)

Lifecycle: `hello {device_id}`, `devices.register`, `state.subscribe`, `session.subscribe` per open session (shared across tabs by the server, resumed without duplicates).

| Method | Purpose |
| --- | --- |
| `hello` | → server version, protocol, `auth` (oidc · dev_token), `user`, `push {vapid_public_key}?`, capabilities |
| `ping` | heartbeat; answers at once |
| `state.subscribe {since: {epoch, rev}?}` | aggregated feed: machines, sessions (`MachineSession`: summary + `machine_id` + `stale`), projects, tasks, contact grants, schedules, devices |
| `session.subscribe` · `unsubscribe` | as in the daemon API, with `machine_id` |
| `session.create {machine_id, …daemon create incl. prompt?, task_ids?}` | create with the first prompt (atomic on the daemon; the controller fills `sandbox` and the prompt `origin`), then link to tasks (moving `backlog`/`todo` tasks to `doing`); idempotent under one `op_id` → the daemon result |
| *pass-through* | every other session, queue, request, todo, job, `sandbox.revoke`, `agents.list`, `fs.list`, `doctor.report`, `settings.*` method, params wrapped in `OnMachine` (`machine_id` next to the daemon fields); unreachable machine → `machine_unavailable` |
| `machines.add` → `add_confirm` · `confirm_host_key` · `remove` · `reconnect` · `upgrade` · `set_settings {name?, key?, auto_upgrade?}` | machines |
| `keys.list` · `rotate` · `generate` · `import` · `delete` | SSH keys |
| `projects.create` · `update` · `delete` | projects (location prefixes) |
| `tasks.create` · `update` · `move` · `delete` · `link` · `unlink` · `checklist_add` · `checklist_update` · `checklist_remove` | tasks (project_ids, links, checklist with item ids) |
| `grants.revoke` | contact grants |
| `devices.register` · `rename` · `remove` | devices (remove signs the device out) |
| `notifications.preferences` · `set_preferences {attention?, questions?, turn_end?}` · `subscribe {device_id, subscription}` · `unsubscribe` | Web Push |
| `schedules.create` · `update` · `delete` · `run_now` | schedules (§8.1) |
| `sandbox.profiles` · `sandbox.set_profile {scope, change}` | sandbox defaults (§11.6) |
| `client.focus {device_id, session?, visible}` | push deduplication |

Notifications: `state.events {changes[{rev, kind}]}` (machine/session/project/task/grant/schedule/device `_upserted` / `_removed`), `session.events` (with `machine_id`).

**Connectivity** (`Machine.connectivity`, by `state`): `connected · connecting · unreachable · incompatible · action_required {reason: host_key_changed|auth_failed|host_key_unconfirmed|controller_conflict, presented_host_key?}`, plus `Machine.update_available`.
UI-only errors: `unauthenticated · machine_unavailable{machine_id, connectivity} · host_key_mismatch · unsupported_platform · key_in_use · invalid_key`.

## 7. TypeScript

`web/src/lib/protocol/`: one file per type, `DaemonMethods.ts` / `UiMethods.ts` (method → `{params, result}`, notification → params), `versions.ts`, `index.ts`.
Regenerate: `CONTROLROOM_UPDATE_TS=1 cargo test -p controlroom-protocol --test typescript`. The `protocol-ts-drift` flake check fails when the checked-in files differ; `web-check` type-checks them.

## 8. Open questions (judgment calls to review)

1. **Unknown fallbacks drop the raw tag.** An older controller re-encodes a newer daemon's `"hibernating"` state as `"unknown"` to the UI. Keeping raw tags would make TS unions non-exhaustive (`string`). OK?
2. **Summary vs. detail split.** Queue items, todos, auto-continue details, agent sessions and jobs are in `SessionDetail` (open session only); the summary keeps counts (`queue.len`, `todo`, `live_work`). Pending `requests[]` and sandbox `grants[]` stay in the summary, as §5.2 lists them.
3. **`job.list` duplicates `SessionDetail.jobs`.** Kept because §5.3 lists it and non-subscribed clients (e.g. `ctl`) need it. Drop?
4. **`session.subscribe` resume** sends an `upsert` of every item changed after `since_seq` instead of replaying ops; no snapshot fallback is needed. There is no paging for very long timelines yet (could be added as `timeline.page` in a minor version).
5. **Raw frames** use `raw_since: Seq` instead of `raw: bool`, so a seq link can open the raw view at a position.
6. **`actor`** is `"root"` or a same-machine child session id. Remote children appear only through `subagent` items and `child_update` deliveries.
7. **Cross-machine references** use `DaemonId` (`PeerRef`) on the daemon side, because machine ids are per controller. The UI resolves them via `Machine.daemon_id`.
8. **`parent_id` vs. `spawned_by`**: `parent_id` is a same-machine parent; `spawned_by` is a parent on another machine. Is that the intended reading of §5.2?
9. **Resolved: one controller per daemon** (replaces the broker claim). A daemon serves exactly one controller; a different `controller_id` gets `controller_conflict`, the same one replaces its old connection, and `debug`-role clients (`ctl`) connect alongside without consuming the mailbox. See §5.
10. **Mailbox flow (provisional, P7)**: the controller raises contact/spawn requests on the session via `inbox.deliver {request}`; the user's answer returns as an outbox `resolved` item; `queued` is the only interim ack. Peer directories are pushed (`peers`), not pulled.
11. **Attention roll-ups** are computed by the daemon over same-machine descendants only; cross-machine roll-ups would need controller support.
12. **Resolved: explicit levels win in either direction.** The controller computes the defaults (strictest level among the machine and matching projects; union of grants, domains and mediated commands) and always fills `session.create.sandbox` with them. An explicit `sandbox_level` (new-session dialog or template) is an affirmative user choice, honored whether stricter or looser, and kept across restarts. Session grants add on top. `SandboxInfo` reports `level`, `default_level`, `explicit` and `looser_than_default` (badge; `off` gets the warning badge).
13. **Mediated commands** are in profiles (`set_mediated` / `remove_mediated` changes), beyond the §11.6 change list, because §11.5b makes them configurable per machine and project.
14. **Removed list methods**: no `machines.list`, `projects.list`, `tasks.list`, `grants.list`, `devices.list`, `schedules.list`; the state feed carries all of them (devices and schedules were added to it).
15. **`DriverKind::Fake`** is on the wire so FakeDriver sessions are representable in tests.
16. **Tool input** is display text (`input?: string`), not structured JSON, to keep agent payloads off the wire. Structured results travel as `output` (e.g. `diff`); anything else is in the raw view.
17. **Auto-continue budget** is a run count (`auto_continue_budget`); dollar budgets (§23) are a later addition.
18. **`fs.list`** has no controller-side cache flag (the prototype's `cached`); add it if offline directory listing is wanted.
19. **Daemon-raised requests in the timeline.** Sandbox approvals have no transcript record. Their `request` item sits at the transcript head when they were raised (`seq_from = seq_to = head`), and each change to them counts as changed *after* the head, so a client resuming with `since_seq = head` still receives it (possibly again: upserts are idempotent).
20. **Sandbox approval options.** File access offers "Allow for this session" rather than "Allow once", because nono remembers a granted path for the rest of its session. Commands and endpoints offer "Allow once" only: nono asks per invocation, and remembering a command grant in the daemon would undercut "approve each time" for `git push`. Granted runtime paths are not listed in `SandboxInfo.grants` yet.
