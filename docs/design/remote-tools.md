# Design: pi on the server, tools on the hosts

Status: accepted (W9s spike, branch `spike/remote-tools`); this note is the rationale. The model is in
`docs/architecture.html`, and the contract in `docs/protocol.md` (source of truth: `crates/protocol`), which
supersedes the protocol sketches of §3 where they differ (R0 kept the shapes but, e.g., routes host
notifications generically through `hostops.notify` instead of `request.raise`; see `docs/protocol.md` §11).
Evidence: `spike/remote-tools/` (throwaway code) and §11 (measurements).

## 0. Decisions in one screen

| # | Decision | Why |
| --- | --- | --- |
| 1 | **pi runs only on the server**, inside a new **agent daemon** (`controlroom-agentd`, own system user, own systemd unit), under nono, with no project files and no route to hosts except its agent socket. | The user's decision. One copy of pi, credentials, extensions and transcripts; hosts stay dumb and cheap. |
| 2 | **Hosts run only `controlroomd` (host role)**: workspaces, named PTY shells, a tool executor with a **journal keyed by tool call id**, jobs, file service, nono per workspace. No pi, Node, bundle or model credentials. | Everything that must touch project files runs where the files are; nothing else does. |
| 3 | **The controller routes** host traffic: agentd ↔ controller over a local socket, controller ↔ host over the existing russh exec channel. The controller keeps sole custody of SSH keys and host trust. | One owner for keys, host-key trust, bootstrap and auto-upgrade (all already in the controller). A web restart only pauses tool calls (they resume by call id). |
| 4 | **A session has workspaces** (`machine:path`, one or more, any machines). Shells belong to a workspace. Two sessions may share a `machine:path` (each has its own shells; the UI warns). | The user's decisions 2 and 4. |
| 5 | **The agent addresses files as `machine:path`.** pi's built-in tools are disabled (`--no-builtin-tools`) and the extension registers `read write edit bash grep find ls` under the same names plus `shell_new shell_list`. | Same names keep pi's renderers, the PiDriver's timeline mapping (diffs, titles, kinds) and the model's habits. Verified in the spike. |
| 6 | **Exactly-once per tool**: reads are idempotent; writes are journaled + atomic + compare-and-swap; `bash` is at-most-once (journal before run, resend attaches, never reruns). | A link blip must never duplicate a side effect; measured in the spike. |
| 7 | **Link loss pauses, it doesn't fail**: in-flight calls wait (grace, default 10 min) and are resent by call id after the reconnect; after the grace the call fails with `connection_lost` and its late result is injected when the host returns. | Blips (tailnet re-key, sshd restart) are seconds; reboots are minutes. Silence is never failure (§1 product goal). |
| 8 | **Projects are task groups.** A session belongs to the projects of its linked tasks; unlinked sessions are "Unassigned". Location prefixes are deleted. | The user's decision 5. |
| 9 | **Sessions are global** (the server owns them); machines only own workspaces, shells, jobs and journals. Mailbox, subagents, attention roll-ups and search become local to the server. | Removes the per-machine session split and most of the cross-machine broker (P7). |

## 1. Topology and roles

```text
Browser ──WS──► controlroom-web (axum: OIDC, UI API, push, static)        server machine (always on)
                  └─ controller: store · machine manager · hub · broker · scheduler · host router
                       │ unix socket: agent daemon API                    │ russh, one connection per host
                       ▼                                                  ▼
               controlroom-agentd (own user, own unit)          exec: controlroomd connect ──► controlroomd (host role)
                 runtime: sessions, queue, reconcile,                     workspaces · shells (PTY supervisors)
                 injection, todos, questions, mailbox,                    tool executor + journal · jobs · fs
                 subagents, job wakes, transcripts                        nono per workspace (shells, jobs, fs worker)
                 supervisors ──► nono ──► pi --mode rpc --no-builtin-tools   approval webhook (loopback)
                                          └─ @controlroom/pi ⇄ run/<id>.agent.sock
                                                                  exec: controlroomd pty attach <shell> (terminals)
                                                                  sftp (human file access)
```

| Piece | Lives on | Why there |
| --- | --- | --- |
| `controlroom-web` + controller | server, hardened `DynamicUser` (as today) | Unchanged role: auth, UI API, client store, keys, machine manager. Adds the **host router**. Restarts on deploys; must not take agents with it. |
| `controlroom-agentd` | server, system user `controlroom-agent`, `StateDirectory`, `KillMode=process` | Runs pi (Node needs W^X, so it can't share web's `MemoryDenyWriteExecute`), owns transcripts and model credentials, outlives web restarts and its own upgrades (detached supervisors from versioned paths, as today). |
| pi + `@controlroom/pi` | server, one nono sandbox per session | Model hosts and the agent socket only. No project files, no SSH, no host network. |
| `controlroomd` (host role) | each execution host, per user, bootstrapped over SSH (as today) | Files, shells and builds are there. Detached PTY/job supervisors survive daemon upgrades; the journal survives link loss. |

**Why the controller routes instead of agentd dialing hosts itself:** the controller already owns sealed keys (master key),
host-key confirmation, bootstrap with verify-run/rollback, auto-upgrade and connectivity state. Duplicating that in agentd
means two key custodians and two connections per host. The cost is that tool calls pause during a web restart (~1-2 s),
which the call-id resend already handles. (Open question Q1 keeps the alternative.)

## 2. Data model

**Server (agentd store, `agent.sqlite`, versioned migrations):**

- `Session {id, name, title, created_at, …, workspaces[], shells[], parent_id?, state, queue, requests, todos, …}`:
  the current `SessionSummary/Detail` minus `cwd` and `machine_id`, plus:
  - `Workspace {id, machine_id, path: RemotePath, root (resolved by the host), primary, added_at, availability}`;
    `availability` = the machine's connectivity (`connected · unreachable{since} · …`), so the UI can say "devbox unreachable"
    on the session without marking the session itself stale.
  - `Shell {name, workspace_id, cwd, state: idle | running{call_id} | waiting_input{call_id} | gone, reset_pending}`.
  - `Job {…, machine_id, workspace_id, log_path: RemotePath}` (the current job record, located).
- Pending host calls (in memory, rebuilt from the transcript at restart): `{call_id, machine_id, op, since}`.
- Agent dirs, credentials, OAuth refresh, pi session files: as today, but under agentd's state dir.

**Hosts (`host.sqlite`, versioned migrations):** workspaces `{id, session_id, path, root, profile}`, shells
`{id = <session>/<name>, workspace_id, supervisor, log, cwd}`, jobs (supervised, plain log), and the **journal**
`{call_id, session_id, op, args_hash, state: running|done|lost, result, output_ref (shell log offsets), acked}`.

**Controller store:** machines, keys, devices, push, grants, schedules (session ids are now global: `schedule.session_id`
without `machine_id`), tasks (`links: [session_id]`), **projects `{id, name, color}`** (no location prefixes).
Project membership is derived: `projects(session) = ∪ task.project_ids for tasks linking the session`; none → **Unassigned**.

**UI state feed (`state.subscribe`)** keeps its shape: machines (connectivity, doctor, version), sessions (global; each
with its `workspaces[]` and per-workspace availability), projects, tasks, grants, schedules, devices, plus
**`overlaps[]`**: `{machine_id, path, sessions[]}` for workspaces shared by several open sessions (the warning).
Sessions no longer go `stale` with a machine; only their workspaces do.

**What moves from host daemons to the server:** sessions, transcripts, the runtime library (queue, reconcile, injection,
todos, questions, auto-continue, subagents, job wakes, mailbox), agent dirs and credentials, pi launch, agent sockets,
the start-fresh history export, idle-stop. **What stays on hosts:** supervisors (now `pty` and `job` kinds only), nono
profiles (now per workspace), the approval webhook, doctor, `connect`, `fs.list`.

## 3. Protocols

Four JSON-RPC APIs, same framing and tolerance rules as today (`docs/protocol.md` §1-2):

| API | Between | Transport |
| --- | --- | --- |
| UI API | browser ↔ controller | WebSocket (unchanged shape) |
| **Agent daemon API** (was "daemon API") | controller ↔ agentd | local unix socket `/run/controlroom-agent/agentd.sock` |
| **Host API** (new) | controller ↔ `controlroomd` host | SSH exec `controlroomd connect` (as today) |
| Agent API | `@controlroom/pi` ↔ agentd | per-session unix socket (as today) |

### 3.1 Agent API additions (extension ↔ agentd)

- `session.info` → `{workspaces[{machine, path, root, home, shell}], shells[], context_files[{machine, path, content}], skills[]}`.
  Pushed again in the `state` notification when it changes (a workspace added, a shell opened, AGENTS.md edited).
- `host.call {call_id, machine, op: HostOp}` → `HostResult`. `HostOp` is a tagged enum with an `unknown` fallback:
  `read {path, offset?, limit?}`, `read_bytes {path, offset?, length?}`, `stat {path}`, `write {path, text|b64, expect_sha256?}`,
  `ls {path, limit?}`, `find {pattern, path, limit?}`, `grep {pattern, path, glob?, ignore_case?, literal?, context?, limit?}`,
  `bash {shell, command, timeout_s}`. Paths are `RemotePath` strings; agentd checks `machine` is one of the session's
  workspaces (an agent can't name another machine) and the host checks the path against the workspace's sandbox.
- `shell.open {name, machine, cwd}` (idempotent by name), `shell.list`, `shell.close {name}`.
- `host.cancel {call_id}`. Notifications: `host.output {call_id, text}` (ANSI-stripped, capped), `host.waiting_input {call_id}`.
- The `call_id` is pi's tool call id (or `<id>/<n>` for the sub-steps of one tool, e.g. edit's stat/read/write), so a
  resend after an extension reconnect is deduplicated end to end.

### 3.2 Agent daemon API (controller ↔ agentd)

The current daemon API, nearly unchanged (it was already session-scoped): `hello`, `machine.subscribe` → renamed
**`sessions.subscribe {since_rev}`**, `session.subscribe`, every command with `op_id`. Changes:

- `session.create {workspaces[{machine_id, path}], name?, parent_id?, sandbox?, prompt?}` (no `cwd`, no `agent`);
  `session.workspace_add|remove {id, machine_id, path}` (the agent is told by a notice; removing a busy workspace is `busy`).
- **Host routing, pull-style (agentd never calls the controller):** `hostops.subscribe {}` streams
  `hostops.request {call_id, machine_id, session_id, method, params}` notifications, including every still-pending call
  on (re)subscribe; the controller answers with `hostops.respond {call_id, result | error}` and forwards
  `hostops.output {call_id, …}` / `hostops.waiting_input`. Duplicate responses are ignored (first wins).
- `hostops.connectivity {machine_id, state, since}` (controller → agentd): drives grace timers, workspace availability and
  the `connection_lost` result.
- Host-raised requests (sandbox approvals, waiting for input, `bash_privileged`) enter as
  `request.raise {session_id, kind, body, host_ref}`; the user's answer goes back through `hostops.request` (`approval.respond`).
- The outbox/inbox mailbox methods disappear (messaging is local to agentd); `outbox.subscribe` stays only for anything
  the controller must deliver (push-worthy events are already in the feeds).

### 3.3 Host API (controller ↔ host daemon)

| Method | Purpose |
| --- | --- |
| `hello {protocol_version, controller_id}` | single-controller rule as today; result: `host_id`, `epoch` (changes when the daemon restarts), capabilities (`shells`, `jobs`, `sandbox_approvals`, `pty`) |
| `workspace.open {workspace_id, session_id, path, sandbox}` | resolve `~`, check the dir, write the nono profile → `{root, home, context_files[], skills[]}` (AGENTS.override.md/AGENTS.md/CLAUDE.md from the root up to `/`, like pi) |
| `workspace.close {workspace_id}` | stops its shells and jobs |
| `shell.open {shell_id, workspace_id, cwd?}` · `shell.close` · `shell.list` | PTY supervisors under the workspace profile (idempotent by id) |
| `tool.run {call_id, workspace_id, op}` | run once; a repeat **attaches** (running) or **returns the journaled result** (done); never reruns |
| `tool.attach {call_id}` | result of an earlier call; `not_found` if it never started |
| `tool.cancel {call_id}` | ETX into the shell's PTY (SIGINT to the foreground group), SIGKILL of that group after 5 s |
| `journal.ack {call_ids[]}` | results the server has logged; the host may drop them (TTL 24 h otherwise) |
| `tool.run` with `op: job_run · job_follow · job_unfollow · job_kill · job_status` · `jobs.subscribe {since}` · `jobs.ack {seq}` | jobs run here (§3.6): commands are idempotent by `call_id` through the call journal; the host appends line counts, follow matches and exits to a persisted job event log, which the server reads from its cursor; wakes are decided by agentd |
| `fs.list {path, include_files?}` | new-session dialog and file browser (human access, unsandboxed, as today) |
| `approvals.subscribe` · `approval.respond {id, decision, reason?}` | nono webhook approvals from workspace sandboxes |
| notifications | `tool.output {call_id, text}`, `tool.waiting_input {call_id}`, `shell.changed`, `jobs.event {seq, …}`, `approval.request` |

Bulk bytes never ride this channel: terminals use `controlroomd pty attach <shell>` channels, the human file browser uses
SFTP, and `read_bytes` is capped (images ≤ 20 MiB). The spike measured why (§11.4).

### 3.4 Exactly-once rules per tool

| Tool | Rule | After a lost reply | After a host-daemon restart |
| --- | --- | --- | --- |
| `read`, `ls`, `find`, `grep`, `stat` | idempotent | resend (dedupe within the journal TTL) | rerun (harmless) |
| `write` | journaled; temp file + fsync + rename; result written before the reply | resend → journaled result | journal says done → result; `running` → compare the file's hash with the intended content: `applied` / `not_applied` |
| `edit` | pi's edit logic in the extension over `read_bytes` (`<id>/read`) + `write` with `expect_sha256` (`<id>/write`) | each step resends by its own id | as `write`; a changed file is a `conflict` ("re-read and retry"), never a lost update |
| `bash` | **at-most-once**: journal row (fsynced) before the command reaches the shell | resend → attach to the running command or its result | with PTY supervisors: re-parse the shell's log from the call's start offset (result recovered); without: `lost` ("may have partly run"), never rerun |
| `shell_new` | idempotent by name | — | the shell is reopened in its last cwd and the next result says its variables were lost (spike) |
| `job` run/follow/unfollow/kill | at-most-once by `call_id` (call journal); the `started` event is logged before the process exists | resend → the same job id / result; never a second job (spike) | the job's supervisor survives; the new daemon replays the event log and keeps scanning (spike); see §3.6 |

agentd logs the tool result into the session transcript (pi's own `tool_execution_end`) and then `journal.ack`s it.

### 3.5 Timeouts, link loss, cancellation, waiting for input

- **Command timeout** (bash `timeout`, default 600 s) is enforced **on the host**, so it holds while the link is down.
- **Link loss:** the controller marks the machine `unreachable` (SSH keepalives, ~15 s) and redials with backoff. agentd keeps
  pending calls; the controller resends every pending `tool.run` after `hello` (same `call_id`). The tool call in pi stays
  open, its output resumes, and the result notes "the link to devbox dropped N times; the result came from the host's
  journal (the command ran once)".
- **Grace** (default 10 min, per machine setting): when it expires the tool returns an error: "devbox unreachable since T;
  the command may still be running there (call X); its result will be delivered when devbox is back". agentd keeps
  polling `tool.attach` after the reconnect and injects the late result (injection origin `tool_late`, like late replies).
- **Host daemon restart** (`epoch` changed): calls without results become `lost` unless their shell's supervisor survived;
  shells are reopened in their last cwd with a reset notice (spike-verified).
- **Cancellation:** pi `abort` → the tool's `AbortSignal` → `host.cancel` → `tool.cancel` → ETX into the PTY. The shell
  survives with its state; the result is the partial output with exit 130. Spike: abort → run settled in 212 ms.
- **Shared terminals:** the user attaches to the same PTY (`controlroomd pty attach`). The agent's command is written to
  a file and the shell runs `__cr_run <call id>` (a function from the integration snippet) that echoes the command
  text for humans, emits OSC 133 `C`, sources the file and lets `PROMPT_COMMAND` emit `D;<exit>` and OSC 7. The
  result is the bytes between `C` and `D`; the spike did the same with echo off.
- **Waiting for input:** the host flags a call when its PTY's foreground process group (bash itself for builtins such as
  `read`) sleeps in a tty read for 2 s with no output (`tcgetpgrp` + `/proc/<pgrp>/wchan`; spike-verified). agentd raises a
  blocking `terminal_input` request (attention + push); the user types in the terminal pane (`pty attach`); the agent
  can't type. Unanswered for 10 min (setting) → ETX, and the result says it timed out waiting for input.

### 3.6 Background jobs across disconnects and restarts

**Where a job runs.** On the host of the workspace it names (`job run {command, at: machine:path}`), in a **detached job
supervisor** (supervisor kind `job`, as today: own session, outlives the host daemon and its upgrades) launched under the
**workspace's nono profile** (job secrets injected there). Output goes to the supervisor's transcript and the plain-text
log (`<root>/.controlroom/jobs/<id>.log`, a `machine:path` the agent reads with `read`/`grep`).

**What the host records, and how.** The host daemon follows each running job's output and appends to a **persisted job
event log** (`host.sqlite` table, one sequence per host; the spike used an fsynced JSON-lines file):
`started {job_id, session, name, log_path, follow?}` (written *before* the process is spawned) · `progress {lines, offset,
matches[]}` (line count, byte offset, follow-matching line numbers of the new complete lines) · `follow {pattern,
buffer_seconds, matches[]}` (a rescan of the lines so far: `from: unseen`) · `unfollow` · `killed {by}` · `exit {exit_code |
signal | lost, lines, duration}` (written once the output is fully scanned). The host never decides wakes and never
waits for the server: the log is complete whether or not anyone is connected. A restarted host daemon replays the log
to rebuild each job's offset, line count and follow state and continues scanning where it stopped.

**How the server resyncs.** agentd keeps, per host, the **cursor** = the last event sequence it consumed. After every
`hello` (link back, controller restart, agentd restart) it calls `jobs.subscribe {since: cursor}`: the host returns the
backlog after the cursor and then streams live `jobs.event` notifications on that connection (events arriving while the
backlog is in flight are queued and merged by sequence). agentd periodically `jobs.ack {seq}`s, so the host can prune
events of ended jobs at or below every subscriber's ack.

**Exactly once.** agentd turns events into wakes and writes, in **one transaction**, the new cursor, the follow matches
still inside their buffer window, and the injections those events produced (the wake is recorded in the same transaction
that queues its injection, as today). So: *no loss* — an event is either consumed with its effects or still after the
cursor and replayed; *no duplicates* — events at or below the cursor are skipped, and wake ids are deterministic
(`exit:<host>/<job>`, `follow:<host>/<job>:<first>-<last>`), so even a replay can't inject one twice. The injection itself
then follows the queue's existing exactly-once rules (tagged lines, in-doubt barrier).

**Idempotent commands.** `job.run`, `job.follow`, `job.unfollow` and `job.kill` go through `tool.run {call_id}` and the call
journal: a resend after a lost reply, a reconnect or an agentd restart returns the original result (the same job id), and
a run whose daemon died between `started` and the spawn shows up as `exit {lost}` rather than a phantom running job.

**Follow buffers across an outage (merge rule).** A job's matches accumulate until `buffer_seconds` after the first
match's **host timestamp**. After a catch-up, every job whose buffer elapsed during the outage gets **one** follow wake with
all its matches (line ranges); matches still unreported when the job exits ride on its **exit wake**; wakes of several
jobs due together merge into one injection (as today). A replay recomputes the same buffers from the same host timestamps.

**What the agent sees while the host is unreachable.** Nothing is invented: no exit wake, no "failed". `job status`
reports the last known state with `host unreachable since T`. Running jobs keep counting as live work, so auto-continue
says "waiting on job <name> (devbox unreachable)" instead of continuing. When the grace (10 min) passes, one notice per
outage is injected ("devbox unreachable since T; its jobs keep running there; their wakes arrive when it is back"). After
the reconnect the backlog produces the real wakes. **Host reboot:** the supervisor died with the host, so the restarted
daemon finds a `started` job without an exit record and a dead supervisor (pid + start time) and logs `exit {lost:
"host rebooted"}`, which becomes one exit wake saying so.

## 4. Sandboxing and approvals

**Server, per session (pi):** generated nono profile (sandbox crate, as today) with `workdir` = an empty per-session
scratch dir; read `/nix/store` and the bundle; allow agent/home/tmp; `unix_socket` = the session's agent socket only;
network = the model routes (credential injection via `cmd://` as today); `--no-builtin-tools`, so pi can't start shells.
No project files, no SSH keys, no host network. The spike ran pi 1.2.0 exactly like this.

**Host, per workspace:** one generated profile per workspace: read-write = the workspace root; read = `/nix/store` plus the
level's reads (relaxed: home config); network = the session's domains; mediated commands (git push → approve, ssh → deny,
…) as in §11.5b; secrets for jobs injected by the host's nono proxy. **Everything the agent triggers on that host runs
inside it:** PTY shells, jobs, and a per-workspace **fs worker** (`controlroomd workspace-exec`, launched under the same
profile) that serves read/write/grep/find/ls, so the unsandboxed host daemon never touches project files on the agent's
behalf. Two sessions on one path get two profiles (each its own grants).

**Approvals:** nono → host webhook (loopback, per workspace URL/secret, as today) → host daemon `approval.request` →
controller → agentd `request.raise` (a blocking `sandbox` request on the session, attention, push) → user → agentd →
controller → host `approval.respond` → nono. Timeout = deny. `bash_privileged` is the same path with kind `privileged`
and runs unsandboxed on the host in the privileged pane. Model credentials never leave the server; host secrets (for
jobs) never reach the server's sandbox.

## 5. Tools the agent gets

All paths are `machine:path`; `~` is the remote home; a relative path resolves against that machine's (primary)
workspace root; a bare path is accepted only when the session has exactly one workspace.

| Tool | Arguments | Result text |
| --- | --- | --- |
| `read` | `path, offset?, limit?` | pi's format: ≤ 2000 lines / 50 KB, `[Showing lines a-b of n. Use offset=…]`; images (png, jpeg, gif, webp, bmp) attached and resized by pi |
| `write` | `path, content` | `Successfully wrote N bytes to devbox:~/…` |
| `edit` | `path, edits[{oldText, newText}]` | pi's edit (exact then fuzzy match, diff details); `conflict` if the file changed meanwhile |
| `bash` | `command, shell?, timeout?` | `[shell devbox · devbox:~/src/api · exit 0]` + output (last 2000 lines / 50 KB) + notes (truncation with the remote log path, timeout, cancel, waiting for input, link drops, shell reset); non-zero exit = error result |
| `shell_new` | `name, at: machine:path` | opens a fresh login shell in a workspace |
| `shell_list` | — | `name: machine:cwd` lines |
| `grep` | `pattern, path?, glob?, ignoreCase?, literal?, context?, limit?` | `[in devbox:~/src/api]` + `path:line: text` |
| `find` | `pattern, path?, limit?` | `[in …]` + relative paths |
| `ls` | `path?, limit?` | entries, dirs with `/` |
| `terminal_read`, `bash_privileged`, `job`, `todo`, `ask_user`, `subagent`, `message`, `notify_user` | as in the spec, with `at: machine:path` / `shell` where they need a place | unchanged |

**System prompt** (the extension sets `systemPromptOptions` in `before_agent_start`, re-read every run, so edits to
AGENTS.md reach the next run as a section diff):

```text
<project_context>  (pi's own section)
<project_instructions path="devbox:~/src/AGENTS.md">…</project_instructions>
<project_instructions path="devbox:~/src/api/AGENTS.md">…</project_instructions>
<project_instructions path="buildbox:~/work/web/AGENTS.md">…</project_instructions>
<workspaces>
You work on remote machines. Every file path is machine:path, e.g. devbox:~/src/api/README.md.
Relative paths resolve against that machine's workspace root; ~ is the remote home.
Workspaces:
- devbox:~/src/api (root devbox:~/src/api; default shell "devbox")
- buildbox:~/work/web (root buildbox:~/work/web; default shell "buildbox")
Shells (bash {shell}): state (cwd, env, venv) persists per shell. Each bash result starts with [shell · cwd · exit].
- devbox: on devbox, now in devbox:~/src/api/src
- buildbox: on buildbox, now in buildbox:/tmp
Use shell_new to open another shell, e.g. for parallel work.
</workspaces>
<cwd>none on this machine: pi runs on the controlroom server. Address files as machine:path.</cwd>
```

Default shell names: the machine name when the session has one workspace there, else `<machine>-<dir>`.
Workspace skills (`.pi/skills`, `.agents/skills` under the root) are listed by the host and rendered by the extension as
an `<available_skills>` section with `machine:path` locations (pi's own skill discovery is local and is turned off).

## 6. What breaks, and how it is handled

| Thing | Finding (spike) | Handling |
| --- | --- | --- |
| pi built-ins | `--no-builtin-tools` plus ours under the same names: the model sees exactly 9 tools, no duplicates; without the flag our registrations override the built-ins cleanly too | Use the flag (pi can't fall back to local tools) |
| Rendering | pi's own renderers (spread from `create*ToolDefinition`) draw our calls in the TUI (`$ cmd`, edit diffs, `read path:range`, `grep /pat/ in …`); HTML export renders by name; PiDriver's timeline mapping is name/args based and unchanged | Keep names and argument shapes identical to pi's |
| Edit diffs | pi's edit runs server-side over remote ops, so `details.diff` and fuzzy matching are pi's; the patch header needed the display path | Done in the extension |
| Images | `read` fetches the bytes and hands them to pi's read tool, which resizes them | Cap `read_bytes` at 20 MiB |
| Context files | pi only discovers local AGENTS.md; the host returns the chain, the extension injects it as `contextFiles` with `machine:path` names | `--no-context-files` |
| `@file` | RPC mode rejects `@file`; pi's TUI expansion is local | The composer resolves `@machine:path` mentions through the controller (host `read`, caps) and attaches text/images |
| pi-lens | Matches `write`/`edit` by name and treats `path` as a local file: its read-guard, formatter and LSP would act on nonexistent local paths | Not loaded server-side (keep-list). Diagnostics later as a host-side op or jobs (Q6) |
| Skills / project `.pi/` settings | Discovered from the local cwd only | Host lists workspace skills (above); project `.pi/` settings and extensions are not loaded (no project trust needed) |
| `exit` in a command | Kills the persistent shell | The host reports the exit status and respawns the shell in the same cwd on the next call, with a notice (spike) |
| Commands that read the terminal | Block on the PTY | Waiting-for-input detection → user types in the pane; timeout |
| Large files | A windowed read near the end of a 60 MB file takes 66 ms (the host scans to count lines) | Per-file line-offset index keyed by (inode, size, mtime) |
| Bulk transfer | JSON + base64 on the control channel: 12 MiB/s (3.7 MiB/s at +10 ms RTT) vs ~47 MiB/s for raw `ssh cat` | Bulk on SFTP / dedicated exec channels; agent reads are windowed anyway |
| pi's cwd | pi groups session files by cwd and shows it in its footer | One scratch cwd per session; `--session-dir` under agentd's session dir |

## 7. What carries over, is rewritten, or is deleted

| Crate / area | Fate | Notes |
| --- | --- | --- |
| `protocol` | **rewrite parts** | Split `daemon` into `agentd` (sessions; mostly a rename) and `host` (new); `SessionSummary` loses `cwd`/machine, gains `workspaces[]`, `shells[]`; `HostOp`; `Project` loses `location_prefixes`; tasks link `session_id`s; new request kinds `terminal_input`, `privileged`; agent API gains `host.*`, `shell.*`, `session.info` |
| `transcript` | **keep** | Used by agentd (sessions) and hosts (PTY and job logs) |
| `supervisor` | **keep + extend** | `agent` kind on the server; `pty` (new, §22) and `job` kinds on hosts; byte records for PTYs |
| `drivers` | **keep** | PiDriver unchanged (tool names kept on purpose); FakeDriver stays |
| `runtime` | **keep + adapt** | Becomes agentd's core: `Host` port splits into a local supervisor port and a `HostOps` port (pending calls, grace, late results); jobs become remote (launch via host, wakes local); `cwd` → workspaces; mailbox goes local (broker outbox disappears); idle-stop counts pending host calls as live work |
| `sandbox` | **keep, used twice** | Profile generation for pi (server: no workdir files, agent socket, model routes) and for workspaces (host: root rw, mediated commands, job secrets). Agent-dir builder and credential helpers move to server use only |
| `daemon` (`controlroomd`) | **split** | → `controlroom-agentd` (server: `serve` with the runtime, agent sockets, credential helper, doctor for nono/Landlock on the server) and `controlroomd` host role (`serve` host API, `connect`, `supervise`, `workspace-exec`, `pty attach`, doctor, webhook). `launch.rs` pi parts move to agentd |
| `client-core` | **keep** | russh, keys, bootstrap/upgrade unchanged; the daemon client speaks the host API |
| `controller` | **adapt** | Machine manager unchanged; new host router (`hostops` bridge), local agentd client; hub/feeds: sessions from agentd (global), machines from hosts; broker/policy simplified (local mailbox, contact policy stays); projects without prefixes; schedules keyed by session id |
| `web` (crate) | **keep** | Unchanged apart from the module's new agentd unit |
| `fake-agent` | **keep** | Drives agentd tests; gains a scripted `host.call` client (like its `agent` hook) |
| `fixture-tool` | **keep** | Mock model unchanged |
| `extensions/pi` | **extend** | Remote tools (port of `spike/remote-tools/extension`, typed against pi's real API), `session.info`, workspace prompt section, skills section |
| `web/` UI | **adapt** | New-session dialog picks several `machine:path`; session header shows workspaces + availability + overlap warning; terminals per shell; rail by project (task groups) + Unassigned; no location prefixes |
| `nix/` | **adapt** | Module gains `services.controlroom.agent` (pi bundle, nono, credentials, `KillMode=process`); hosts drop `controlroom-pi`; VM e2e reshaped (§8) |
| **Deleted** | | Location prefixes; per-host pi launch (`agents.toml` on hosts, host `sandbox.toml` model routes, OAuth helper on hosts); `machine.subscribe` session summaries from hosts; outbox/inbox broker for cross-machine messages; `spawned_by`/cross-machine child plumbing; host-side history export |

## 8. Implementation plan

Lanes run in parallel within a phase; every merge is gated on `nix flake check`.

| Phase | Lanes | Done when |
| --- | --- | --- |
| **R0 Contracts** | (a) protocol: agentd/host/agent API types, `HostOp`, workspaces, projects-as-groups, TS regen; (b) spec + protocol docs final | Types merged, drift check green, UI builds against them |
| **R1 Foundations** | (a) **host daemon**: host API server, workspaces, PTY supervisor kind + shells (OSC 133/7), tool executor, journal (`host.sqlite`), `fs` worker under nono; (b) **agentd** binary: runtime moved behind a local socket, `HostOps` port with pending calls/grace/late results, sessions with workspaces; (c) **controller host router** + agentd client; (d) **extension remote tools** (from the spike) with unit tests against a fake agentd | Each crate's tests; the spike scenarios (a)-(f) as Rust integration tests: fake agent (pi RPC) → agentd → controller → host daemon over a local "SSH" bridge, with link-drop and daemon-restart steps |
| **R2 End to end** | (a) VM e2e rewrite: server VM runs web + agentd + pi bundle + mock model; devbox/buildbox run only controlroomd + nono; scenarios: two-workspace session, link blip mid-bash (`block()`), host reboot (shell reset notice), approval from a host sandbox; (b) UI: workspaces, overlap warning, project rail, terminals per shell; (c) acceptance on the dev machine with real pi under nono (as the spike) | VM e2e green; Playwright scenarios pass |
| **R3 Runtime parity** | (a) jobs on hosts (wakes local); (b) subagents with workspace inheritance; (c) mailbox local + contact policy; (d) waiting-for-input + `bash_privileged` + `terminal_read`; (e) skills and composer `@machine:path` | Each primitive's acceptance scenario passes on the new topology |
| **R4 Deploy** | (a) NixOS module `services.controlroom.agent`: static user/group `controlroom-agent`, `StateDirectory`, `KillMode=process`, no `MemoryDenyWriteExecute` (Node), Landlock available, the `controlroom-pi` bundle and nono on its `PATH`, model credentials via `LoadCredential`/sops; the agentd socket shared with the web unit through a common `SupplementaryGroups` and `BindPaths`; both units in `backupServices`. (b) nix-config: the server enables both services and holds the model routes (`sandbox.toml` routes move from the hosts' home-manager to the server); hosts drop `controlroom-pi`, `agents.toml` and the model routes and keep nono. (c) Delete the host-side pi paths in code | Running on the server; hosts carry no pi |

## 9. Open questions (resolved)

**All recommendations below were accepted by the user.** In addition: OAuth subscriptions reach the server through a one-time `controlroom-agentd login <provider>` CLI (agentd is the only refresher; a UI flow later), and host-side pi support is deleted outright in R4 (no compatibility mode).

1. **Who dials hosts: controller or agentd?** Recommend the controller (single key custodian; agentd survives web
   restarts and tool calls only pause). Alternative: move the machine manager into agentd and make web a pure UI.
2. **Grace before `connection_lost`:** recommend 10 min per machine (setting), with the late result injected afterwards.
3. **One binary or two?** Recommend two (`controlroom-agentd` on the server via Nix, static `controlroomd` for hosts),
   sharing crates; host auto-upgrade stays independent of server deploys.
4. **Host search tools:** recommend embedding ripgrep's crates (`ignore`, `grep-searcher`, `globset`) in `controlroomd`,
   so hosts need nothing but bash and nono (the spike used `rg`/`fd` from the VM).
5. **Confinement of file tools:** recommend workspace roots read-write and the level's read set read-only (same as the
   current strict/relaxed), enforced by the fs worker running under the workspace's nono profile.
6. **pi-lens:** recommend dropping it from the server keep-list now and offering host-side diagnostics later.
7. **Shell state after a host reboot:** recommend "reopen in the last cwd + notice" (spike) rather than replaying
   history; the user can keep a venv activation in a project `.envrc`-style init file (later).
8. **Overlapping workspaces:** recommend warn-only (decision 4) plus a per-file CAS conflict on edits; no locks.
9. **Session in several projects:** derived from its tasks, so possible; the rail shows it under each.
10. **Protocol version:** nothing is deployed, so recommend redefining the APIs in place as 1.0 (per §2 Compatibility).
11. **Model credentials on the server:** API keys via sops → `LoadCredential` into agentd (never in the sandbox, as today);
    OAuth subscriptions need a one-time login on the server. Recommend a `controlroom-agentd login <provider>` CLI first
    and a UI flow later; agentd stays the only refresher.
12. **Creating a session while a workspace's host is unreachable:** recommend refusing (`machine_unavailable`) for the
    first release; lazy workspaces later.
13. **Server sizing:** an idle pi (with the extension) is ~120 MB RSS on the dev machine, so 20 live sessions need
    ~2.5 GB plus nono; recommend keeping idle-stop on (48 h) and exposing live pi count in doctor.
14. **Search, previews, files:** search across timelines becomes server-local (simpler); dev-server previews and the
    file browser still go to the host (direct-tcpip, SFTP) through the controller, unchanged.

## 10. Spike: what was built

`spike/remote-tools/` (not for merge):

- `hostd/jobs.py`: background jobs: a detached wrapper (`hostd.py job-exec`, own session, own process group for the
  command, `exit.json` on exit) standing in for the job supervisor; a scanner and the fsynced job event log
  (`started/progress/follow/unfollow/killed/exit`, one sequence), replayed when the daemon restarts; `jobs.subscribe {since}`.
- `server/jobwakes.mjs`, `server/store.mjs`: the server side of jobs: per-host cursors, follow buffers and wakes in one
  atomically written state file (so the router can be killed and restarted), deterministic wake ids, the merge rule.
- `hostd/hostd.py`: host daemon stand-in (Python, so the VMs carry no Node): `connect` bridge that starts a detached
  `serve`; workspaces with the AGENTS.md chain; named PTY bash shells with OSC 133/7 markers, ETX interrupt, `exit`
  respawn and waiting-for-input detection; read/read_bytes/stat/write (atomic, CAS)/grep (rg)/find (fd)/ls; a journal
  keyed by call id (`tool.run` idempotent, `tool.attach`, `lost` after a restart).
- `server/router.mjs`: agentd + controller stand-in: the agent socket, SSH exec channels (`ssh … crspike-hostd connect`,
  the intended path is russh in the controller), redial with backoff, resend by call id, grace, shell reopening after a
  daemon restart, an admin socket for link drops.
- `extension/index.ts`: the pi extension (the tools above, renderers reused from pi, context files and the workspaces section).
- `nix/vms.nix`, `scripts/vms.sh`: two NixOS VMs (devbox, buildbox) with sshd, alice, Python, rg, fd; SSH on
  127.0.0.1:47221/47222; throwaway key under `/var/tmp/crs`.
- `run-spike.mjs`: drives real pi 1.2.0 **under nono 0.80.0 on the dev machine** (x86_64 NixOS, kernel 6.18, Landlock) (profile: empty workdir, agent socket only,
  mock model route only) with the fixture tool's mock model scripting the tool calls; 45 checks; `evidence/`. Job wakes
  are injected into pi by the driver as prompts (agentd's injection path stands in).

Run: `spike/remote-tools/scripts/vms.sh build && spike/remote-tools/scripts/vms.sh start &&
node spike/remote-tools/run-spike.mjs; spike/remote-tools/scripts/vms.sh stop` (needs `nix build .#nono -o
/var/tmp/crs/gc-nono` and `cargo build -p controlroom-fixture-tool`).

## 11. Spike results

All 45 checks passed (`evidence/results.json`; router log of the job resyncs and wakes in `evidence/jobs-router-log.txt`;
TUI replay of the earlier, pre-jobs session in `evidence/tui-replay.txt`).

### 11.1 Behavior (a)-(f)

- (a) `read devbox:~/src/api/src/main.rs`, `write buildbox:~/work/web/notes.md`, `edit devbox:src/main.rs` (relative to
  the workspace; pi's diff in `details`), image read (png attached), windowed read at line 500 000 of a 1M-line file; a
  bare path with two workspaces is refused with a hint; a failing edit names `devbox:~/…`.
- (b) Shell `devbox`: `cd src && export GREETING=… && greet() {…}` then `pwd; echo $GREETING; greet there` →
  `[shell devbox · devbox:~/src/api/src · exit 0]`; `devbox-2` on the same workspace is independent; `buildbox` keeps
  `cd /tmp` and exports; a venv activation persists; `exit 3` reports exit 3 and the shell restarts fresh in the same cwd.
- (c) grep/find/ls run on the hosts (rg/fd); a grep with `limit 5` over the 60 MB file takes 27 ms.
- (d) With `--no-builtin-tools` the model is declared exactly `read write edit bash shell_new shell_list grep find ls job`;
  without the flag, the same (ours override; checked before `job` was added). TUI replay renders every call with pi's renderers.
- (e) The system prompt carries `devbox:~/src/AGENTS.md`, `devbox:~/src/api/AGENTS.md` (parent chain) and
  `buildbox:~/work/web/AGENTS.md`, the `<workspaces>` section, and no local cwd.
- (f) **Link blip:** an 8 s loop on devbox; after `tick 2` the router's SSH client is SIGKILLed and kept down 3 s. The
  call stays open, the router redials, resends `tool.run` with the same id, the host attaches it to the running command:
  the tool returns after 8.1 s with all ticks, `ran.txt` shows one run, and the result tells the agent about the drop.
  **Outage longer than the grace (20 s in the spike):** the tool fails with "devbox unreachable since …"; after the
  reconnect `tool.attach` still returns the result (`outlived`, exit 0) from the journal. **Host daemon killed
  mid-command:** the resend returns `lost` ("the host daemon restarted while this call ran"), the command ran once,
  and the next bash in that shell runs in its last cwd with a reset notice. **Abort:** Ctrl-C reaches the remote
  command; abort → settled in 212-255 ms; the shell keeps its state. **CAS:** a write against a stale hash is refused.

### 11.2 Background jobs (brief addendum)

| Scenario | What happened | Checks |
| --- | --- | --- |
| **Link cut** | Job `build` on devbox: 10 s loop, `ERROR` lines at iterations 2, 6, 7, `follow: ERROR`, buffer 2 s. The first follow wake (line 3) arrives live; then the router's SSH link is killed and kept down 5 s. 2.5 s into the outage the job had already logged 8 lines on the host; at the end 14. After the reconnect the router resynced from event 4 (5 new events) | one merged follow wake for lines 8 and 10 (both produced during the outage), 5.3 s after the cut; exactly **one exit wake** ("exited with 0 after 10.2s, 14 lines"); the host log has one `exit` event; the agent received the exit wake once |
| **Retried `job.run`** | `tool.run` again with the agent's call id `j1` and the same command | same job id returned; `job-ran.txt` shows one start |
| **Server restart** | Job `tests` (8 s, `ERROR` at 4, 5); the router (agentd stand-in) is SIGKILLed right after the start and restarted 5 s later from its state file; it resynced from event 14 (6 new events) | one merged follow wake (lines 5-7) and one exit wake; total wakes 3 → 5 (no earlier wake repeated, all ids unique); the job ran once; the agent got each exit wake once, the earlier job's still once |
| **Host daemon restart** | Job `lint` (6 s); the host daemon is SIGKILLed 2 s in; the job's wrapper survives; the router redials, the new daemon replays the event log and keeps scanning; resync from event 27 (2 new events) | one exit wake with all 7 lines; one `exit` event on the host; the job ran once |

Not run: a real host reboot (the code path logs `exit {lost}` when the wrapper's pid/start time is gone) and the grace
notice for jobs on a long-unreachable host (design only).

### 11.3 Latency (g), through router → SSH → host daemon (VMs on the dev machine's loopback)

| Call | p50 | p95 | +10 ms RTT p50 / p95 |
| --- | --- | --- | --- |
| host daemon `hello` round trip (transport floor) | 0.3 ms | 0.3 ms | 11.7 / 12.9 ms |
| `stat` | 7.4 ms | 10.0 ms | 18.3 / 20.6 ms |
| `read` small file | 7.8 ms | 10.1 ms | 21.1 / 24.6 ms |
| `bash true` in a named shell | 3.9 ms | 9.6 ms | 20.1 / 26.2 ms |
| `read` offset 900 000 of a 1M-line file | 67 ms | 69 ms | 79 / 85 ms |

Numbers are from the final run (with the job scanner running in the host daemon; an earlier run without jobs had
`stat` 2.5/4.2 ms and `read` 3.7/8.1 ms, so expect a few ms of run-to-run noise on a busy machine).
Inside pi (`tool_execution_end.durationMs`, includes the extension and agent socket): ls 13 ms, bash 19 ms (p50),
job run 24 ms, write 25 ms, edit 26 ms (three host round trips), find 29 ms, grep 75 ms. Per-call overhead over the network RTT is ≈ 5-7 ms in
the spike (Python host daemon, an extra Python `connect` bridge, Node router); a Rust host daemon should be lower.

### 11.4 Throughput

64 MiB through `read_bytes` (4 MiB chunks, base64 in JSON lines): 5.4 s = **11.9 MiB/s** locally, **3.7 MiB/s** at
+10 ms RTT; raw `ssh cat` of the same file: 1.31 s (~49 MiB/s). Conclusion: control channel for control and windowed
reads, separate channels for bulk.

### 11.5 Not covered by the spike

nono on the hosts (the existing profile work applies unchanged per workspace); PTY supervisors that survive a host
daemon upgrade (the existing supervisor design; the spike's shells live in the daemon process, hence `lost` on restart);
russh in the controller (the spike used the `ssh` client with the same exec-channel shape); a real network (latency was
shaped with `tc netem` inside the VMs).
