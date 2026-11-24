# Design: profiles as tenants

Status: **accepted; P0 (contracts), P1a (controller) and P1b (agent daemon) done; P1c (UI) next.** The protocol
types and methods are in `crates/protocol` and `docs/protocol.md` (§"Profiles"). The controller implements its side
(store, `profiles.*`, C1–C13, reports to the agent daemon, push redaction); the agent daemon enforces its part
(A1–A4, A6–A9; A5's inheritance) and answers `profiles.report` and `session.move` (§12).
The sandbox defaults rename (decision 9) is done: `sandbox.defaults`, `sandbox.set_defaults`, `SandboxDefaults`,
`DefaultsScope`, `DefaultsChange`, store table `sandbox_defaults`.

It builds on `docs/architecture.html` (§2 Projects, §5 protocol, §8 subagents and mailbox, §10 contact policy,
§11 sandboxing, §12 controller, §13 UI) and replaces parts of the S2 spike's trust-domain design (§13 maps what is
kept). Design lane D1 came with a throwaway UI prototype (branch `design/profiles`), which is not for merge.

**User decisions.** Every recommendation of §14 is accepted:

1. pi's server sandbox is always on; level off unsandboxes only the hosts.
2. A shared machine entry exists only by explicit choice, only between non-private profiles, and never at level off.
3. Machine names are unique per owner.
4. Private means metadata-only push, no shared machines and an explicit account list.
5. Moving a session between profiles is explicit, and it restarts a live agent.
6. With several profiles there is no Unassigned entry in the rail.
7. Deleting a profile requires it to be empty, except archived sessions, which are deleted with it.
8. The private home comes later (P3).
9. The sandbox's "profiles" are renamed "defaults", so "profile" means only the tenant.
10. SSH keys stay global; the server-wide session settings move to General.
11. The notifications panel under ∗ aggregates every profile; a project view stays scoped.

**Start fresh, no migrations** (this replaces §0 decision 7 and the upgrade parts of §4 and §6). The profiles
release changes the controller's client store (and agentd's session store) with a schema bump and no upgrade path.
An older database is refused with the existing "delete the database file to start over" message; `docs/deployment.md`
says what to redo. There is no built-in "Default" profile: a fresh store starts with **no profiles**, and creating
the first one is part of setup (§6).

**Profiles are fully user-configurable.** No profile names, colors or "types" exist in code: no presets, no
special-casing. "Private" is a boolean on any profile; model access is a per-profile setting (any, or a list of
accounts). Names such as Work, Personal and Assistant in this document are examples only; tests and fixtures use
arbitrary names.

## 0. Decisions in one screen

| # | Recommendation | Why |
| --- | --- | --- |
| 1 | A **profile is a tenant** (model B). It owns machine entries, which model accounts its agents may use, projects and their tasks, sessions with their schedules, sandbox defaults and contact. | The user's decision. One controlroom serves work, personal and a private assistant without their agents ever meeting. |
| 2 | **Host network policy is not controlroom's concern.** controlroom connects as the machine entry's user; per-user firewalls or proxies live in the user's nix-config. | The user's decision. The S2 egress proxy, nftables module and approval relay are dropped. |
| 3 | A **machine entry belongs to one profile**. It may be **shared** with other non-private profiles by an explicit choice; a private profile's entries are never shared. A shared entry never runs at level off. | The uid is the boundary between profiles, so sharing an entry (one uid) is a deliberate weakening, and the sandbox must stay on. |
| 4 | A profile's **model allow-list names accounts** (pi provider ids: a preset, each of its named accounts, a gateway, a server-declared provider), or is **Any**. An account can be enabled in several profiles. A private profile lists its accounts, never Any. | The credential decides who sees the data, so the account is the unit. Any keeps today's behavior for the default profile. |
| 5 | **agentd enforces models; the controller enforces membership.** Every rule is enforced at least once on a server; UI scoping is a convenience, never the only check. | Agents reach agentd (pi, the extension) and the controller (through agentd), never the UI. |
| 6 | **pi's server sandbox is always on**; a session's level only shapes its host sandboxes. | At level off pi runs as the agent daemon's user. It could then read every profile's transcripts and credentials, and the model allow-list could not hold. |
| 7 | ~~Additive migration into a Default profile.~~ **Superseded by the user: start fresh.** The stores are recreated with no profiles; the first one is created during setup. Moving machines, projects (with their tasks) and sessions between profiles is one "Move to profile" action. | No migrations before the first release, and no built-in profile. |
| 8 | **UI: one profile looks exactly as today.** With several profiles, the rail groups projects by profile and every view is scoped to the current profile. Notifications and the bell aggregate every profile, tagged. | The user's decision (§10). |
| 9 | **Private profile** means metadata-only push, no shared machines and an explicit account list. A lock marks it in the rail, on chips and in the picker. | Private data must not reach push services or unlisted model providers. |
| 10 | The **private-home sandbox** (a `HOME` per profile on machines where the agent runs as the user) comes later (P3, after a spike). | Dedicated-user hosts don't need it, and work machines are usually single-profile. It changes nono profile generation. |

## 1. The model

A **profile** is a named tenant of one owner (the signed-in user): "Work", "Personal", "Assistant". Profiles
separate *agents*, not people: the owner sees and acts on every profile, and push, devices and keys stay the
owner's.

| A profile owns | How |
| --- | --- |
| **Machine entries** (host + user) | `machine.profile_ids`: one profile, or several when shared (never with a private profile). The same physical host appears in two profiles as two entries under two users (`alice@devbox` in Work, `agent@devbox` in Personal). |
| **Model accounts** it may use | `profile.models`: Any, or a list of account ids. Accounts themselves (keys, sign-ins, gateways) stay in the agent daemon's Settings › Models store, shared by every profile that enables them. |
| **Model defaults** | `profile.model_defaults` (provider, model, thinking). Unset: the server-wide defaults, as today. |
| **Projects and tasks** | `project.profile_id`; `task.profile_id` (a task's projects are all in its profile; a task without a project carries its own). |
| **Sessions** | `session.profile_id`, set at creation (a child's is its parent's). Every workspace's machine is in the profile. |
| **Schedules and job wakes** | Through their session (no own field). |
| **Sandbox defaults** | A new `profile` scope next to `machine` and `project` (level, grants, never-paths, domains, mediated commands). Its machines' and projects' defaults are the profile's too. |
| **Contact** | Peers, contact requests, grants, task links and the mailbox never cross profiles, with no override. |

**Global (the owner's):** SSH keys (the controlroom key and additional keys), devices, notification preferences and
push subscriptions, General (theme, keyboard), and the agent daemon's server settings (idle stop, retention).
Model *credentials* are global objects; which profile may use them is the profile's.

**Private profile** (`profile.private`, a lock icon):

- push payloads are metadata only ("Assistant: needs you"), with no session name or text;
- its machine entries can't be shared;
- its model policy can't be Any;
- it is left out of digest pushes (the away summary, when built).

In-app views show everything as for any profile: it is the owner's own browser.

**What a profile is not.** It is not an access-control boundary between humans (owners stay separate as today) and
not a network policy. Server-side transcripts of every profile live under the agent daemon as today (§12,
decision 11).

## 2. Enforcement

Each row says what is refused, where, and the error. `policy_denied {profile_id, reason}` is a new error kind; the
message names the session's own profile and never names another.

### agentd (model policy and agent-reachable paths)

| # | Point | What is refused | How |
| --- | --- | --- | --- |
| A1 | Launch (`launch::prepare`) | Every provider outside the profile's accounts. | The S2 spike's `ProviderAllowList` (commit `77a513e`): unlisted providers are left out of the session's `models.json`, `auth.json` and default provider. They get no credential route, so the server sandbox can't reach their hosts, and no credential-manifest entry. pi answers `Unknown provider` (tested against real pi). |
| A2 | Launch, level | pi without its server sandbox. | pi always runs under nono on the server (decision 6). The spike's `ModelPolicyNeedsSandbox` refusal becomes unreachable and is not ported. |
| A3 | `session.set_option` (model) | A model of an account outside the list. | `policy_denied` "Not available in Assistant". pi doesn't list such models anyway; this covers a stale UI. |
| A4 | Defaults | A default provider outside the list. | Dropped at launch (spike), so pi starts on an allowed model. The controller refuses such defaults when they are set (C9). |
| A5 | Subagents (R3) | A child whose `model` is outside the list; a child workspace on a machine outside the profile. | A child inherits `profile_id` and the policy. Both are refused before launch with a tool error, not raised as a spawn request: the user couldn't allow them either. |
| A6 | Credential capture | `controlroom-agentd credential <session> <provider>` for an unlisted provider. | It is not in the session's manifest, so the answer is `UnknownProvider` (spike test). |
| A7 | Environment | Provider keys reaching pi through the environment. | The unit strips the variables pi reads as provider keys (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, …; the list comes from pi's env-key table and is pinned with pi) at startup, as it strips `NONO_*`. |
| A8 | Host calls (`host.call`) | A call to a machine that is not in the session's profile any more (it was moved). | `policy_denied`, checked against the machine report (`hostops.machines` gains `profile_ids`). Today's check that the machine is one of the session's workspaces stays. |
| A9 | `session.create`, `workspace_add` | A workspace on a machine outside the profile. | Defense in depth behind C1/C2, from the same machine report. |
| A10 | Mailbox (R3) | Delivery between sessions of different profiles. | An equality check on delivery. Listing (`peers`) already hides other profiles. |

### Controller (membership and owner-side data)

| # | Point | What is refused | How |
| --- | --- | --- | --- |
| C1 | `session.create` | A workspace machine outside the session's profile. A missing `profile_id` when the machines don't name exactly one profile. | `policy_denied`; `invalid_params`. The UI always sends the profile. |
| C2 | `session.workspace_add` | A machine outside the session's profile. | `policy_denied`. |
| C3 | `tasks.create` / `tasks.update` | Projects of several profiles, or of another profile than the task's. | `policy_denied`. |
| C4 | `tasks.link` | Linking a session of another profile (a link grants contact). | `policy_denied`. |
| C5 | Peers and contact grants | A grant between profiles; a peer of another profile in the directory the controller gives agentd. | Grants are refused (`policy_denied`; the UI can't produce one). The peer directory is filtered per profile. |
| C6 | `schedules.*` | Nothing extra: a schedule belongs to its session, so to its profile. | Listing in the UI is per profile. |
| C7 | `machines.set_settings {profile_ids}`, `machines.add_confirm` | An empty list; a private profile with another profile on the same entry; sharing an entry of a private profile. | `invalid_params` / `policy_denied`. |
| C8 | Effective level | Level off on a shared machine entry. | `session.create` and `set_sandbox_level` refuse with `policy_denied` "devbox is shared with Personal; it can't run unsandboxed". |
| C9 | `profiles.update` | A private profile with models Any; model defaults outside the list. | `invalid_params`. |
| C10 | `profiles.delete` | A profile with machines, projects, tasks or open sessions. | `invalid_state`. Its archived sessions are deleted with it (the request carries `delete_archived: true`, the UI confirms with the count). |
| C11 | Push | Session names and text of private profiles. | The payload carries the profile, the attention kind and the session id (for the deep link) only. |
| C12 | `sandbox.set_profile` (renamed `sandbox.set_defaults`, §4) | A `profile` scope that doesn't exist. | `not_found`. |
| C13 | `profiles.move` (§6) | Moving a busy session; a session whose machines don't end up in the target; a task split across profiles. | `busy` / `policy_denied`, listing what blocks. |

### Hosts

Nothing new. Each machine entry is its own SSH user and its own `controlroomd`. The uid is the boundary between
profiles on a host, and per-user network rules (if any) are the user's nix-config.

### UI

Every view is scoped to the current profile (§10). This is never the only enforcement: a stale or forged client
still meets C1–C13 and A1–A10.

## 3. Model policy details (question 2)

- **The list names accounts.** An account is a pi provider id: a preset's first account is the preset id
  (`anthropic`), added accounts have their own (`anthropic-work`), gateways and server-declared providers are
  their names. "Anthropic, any account" is not expressible on purpose: a new account must not silently join a
  profile.
- **Any** (`{"kind": "any"}`) admits every account, future ones included. A new non-private profile starts with
  it, so a single profile behaves as today; a new private profile starts with no account.
- **Several profiles may enable the same account**, e.g. a self-hosted gateway in Personal and in Assistant. Usage
  stays per account. Per profile and account is a later refinement (usage buckets are per session, and a session
  has a profile).
- **A private profile lists its accounts.** It is free to list a paid one: that is the user's choice, not a rule.
- **Defaults** layer as today, with one more layer on top: the server's `settings.json`, then Settings › Models
  defaults (server-wide), then the profile's. A profile's defaults must be on its list (C9). A refused default is
  dropped at launch (A4).
- **Changes apply to new sessions and restarts**, like sandbox defaults. `set_option` and subagent checks use the
  current policy at once.
- **Where the policy lives.** The controller owns profiles, so it owns the policy, and it reports it to agentd as it
  reports machines: `profiles.report {profiles: [{profile_id, models, model_defaults, private}]}` after `hello`
  and on every change. agentd persists the last report (`profile_policies`), so launches after a restart don't
  wait for the controller. An unknown profile id admits no account (fail closed).

## 4. Protocol and store (question 5)

### Protocol (`crates/protocol`, TS regenerated)

| Area | Change |
| --- | --- |
| ids | `ProfileId` (ULID string), generated when a profile is created. No fixed or built-in id. |
| `ui::Profile` (new module `ui/profiles.rs`) | `Profile {id, name, color, private, models: ModelPolicy, model_defaults: ModelDefaults, position, created_at}` (all-`null` model defaults: the server's); `ModelPolicy` (in `agentd`, shared) tagged `any` / `only {accounts}` / `unknown` (admits nothing: fail closed). |
| UI methods | `profiles.list`, `profiles.create {name, color, private?, models?}`, `profiles.update {profile_id, name?, color?, private?, models?, model_defaults?, position?}`, `profiles.delete {profile_id, delete_archived?}`, `profiles.move {profile_id, machine_ids?, project_ids?, session_ids?, share?, dry_run?}` → `{task_ids, session_ids, unlinked, revoked_grants, stranded}` (§6). |
| State feed | `StateSnapshot.profiles`; `StateEvent::ProfileUpserted {profile}` / `ProfileRemoved {profile_id}`. Older clients ignore them (`Unknown`). |
| `Machine` | `profile_ids: Vec<ProfileId>` (≥ 1; > 1: shared; empty only from a P0 controller). `NewMachine` and `MachinesSetSettingsParams` gain `profile_ids`. |
| `Project`, `Task` | `profile_id: Option<ProfileId>` (`null` only from a P0 controller). `ProjectsCreateParams.profile_id`; `TasksCreateParams.profile_id` (required without projects); both may be omitted while the owner has exactly one profile. |
| `SessionSummary` | `profile_id: Option<ProfileId>`, from agentd (`null` only from a P0 agent daemon). The `FeedSession` carries it unchanged. |
| `Schedule`, `ContactGrant` | Nothing: they follow their sessions. |
| UI `session.create` | `profile_id: Option<ProfileId>` (C1). |
| agentd `session.create` | `profile_id`, filled by the controller (a new `Fill::Profile`); a child's is its parent's. |
| agentd | `profiles.report` (§3); `session.move {op_id, id, profile_id}` (§6); `hostops.machines` entries gain `profile_ids`. |
| Errors | `policy_denied {profile_id, reason}`. |
| Push | A protocol type, `ui::PushPayload {session_id, profile_id, state, url}` plus its content, tagged `kind`: `session {name, machine}`, or `private {profile}` (metadata only, C11). The service worker titles a private push with the profile's name and the attention kind. |
| Sandbox defaults | Renamed in P0 (done): `SandboxDefaults`, `DefaultsScope`, `DefaultsChange`, `sandbox.defaults` and `sandbox.set_defaults` (formerly `SandboxProfile`, `ProfileScope`, `ProfileChange`, `sandbox.profiles`, `sandbox.set_profile`), so "profile" means one thing. `DefaultsScope` gains `profile {profile_id}`. |

### Client store (controller, `schema.rs`): schema bump, no upgrade

The schema is edited in place and its version bumped; an older database is refused ("delete the database file to
start over"). P0 removed the store's in-place upgrade machinery and renamed `sandbox_profiles` to
`sandbox_defaults` (version 7). P1 adds, in the schema itself:

```sql
CREATE TABLE profiles (
    id              TEXT NOT NULL,
    owner_id        TEXT NOT NULL REFERENCES owners(id),
    name            TEXT NOT NULL,
    color           TEXT NOT NULL,
    private         INTEGER NOT NULL DEFAULT 0 CHECK (private IN (0, 1)),
    models          TEXT NOT NULL DEFAULT '{"kind":"any"}',   -- ModelPolicy JSON
    model_defaults  TEXT,                                     -- ModelDefaults JSON; NULL: the server's
    position        REAL NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (owner_id, id),
    UNIQUE (owner_id, name)
) STRICT;

CREATE TABLE machine_profiles (
    owner_id    TEXT NOT NULL,
    machine_id  TEXT NOT NULL,
    profile_id  TEXT NOT NULL,
    PRIMARY KEY (machine_id, profile_id),
    FOREIGN KEY (machine_id, owner_id) REFERENCES machines (id, owner_id) ON DELETE CASCADE,
    FOREIGN KEY (owner_id, profile_id) REFERENCES profiles (owner_id, id) ON DELETE RESTRICT
) STRICT;

-- In the existing tables (no defaults: every row names its profile):
--   projects: profile_id TEXT NOT NULL, FOREIGN KEY (owner_id, profile_id) REFERENCES profiles (owner_id, id)
--   tasks:    profile_id TEXT NOT NULL, the same foreign key
--   sandbox_defaults: CHECK (scope_kind IN ('machine', 'project', 'profile'))
```

- With the columns in the schema itself, `projects.profile_id` and `tasks.profile_id` carry real foreign keys, so
  `profiles.delete` can't strand a row.
- Machine names stay unique per owner (`UNIQUE (owner_id, name)` unchanged; decision 3 in §14).
- Schedules and grants need nothing. Sessions are agentd's.
- Tests: a fresh database gets the new version with no profiles; an older one is refused.

### Agent daemon store (`agent.sqlite`, schema bump, no migration)

- Like the client store, the schema is edited in place (`crates/runtime/src/store/schema.rs`, version 2; version 1
  had a migration list, now gone). Any other version is refused with "delete the agent daemon's state to start
  over".
- `sessions.profile_id TEXT` (set at creation; `NULL` only for a session created while no profile was reported: a
  debug client's, or one from a controller before profiles. Such a session is unrestricted while no profile is
  reported, and admits no account and no machine once one is; `session.create` without a profile is then refused).
- `CREATE TABLE profile_policies (profile_id TEXT PRIMARY KEY, models TEXT NOT NULL, model_defaults TEXT NOT NULL, private INTEGER NOT NULL, reported_at INTEGER NOT NULL)`.
- Children are created with their parent's `profile_id`. A session whose profile has no reported policy admits no
  account (fail closed).

### Which agentd APIs need the profile

They are `session.create` (and children), `session.move`, `profiles.report`, `hostops.machines` (profile ids per
machine) and `SessionSummary.profile_id`. The rest stays profile-blind, because a session id implies its profile:
prompts, requests, queue, todos, jobs, `models.*` (accounts are global; their use is per profile) and
`settings.*`.

## 5. Machines (question 1)

- **One profile per entry by default.** The same host in two profiles is two entries (two users, two daemons,
  two SSH connections). The single-controller rule and `UNIQUE (owner, host, port, user)` already make one
  host-user one entry.
- **Shared entries** (`profile_ids` with several profiles) are an explicit choice in the machine card ("Share
  with…"), e.g. a build box used by Work and Personal as the same user. A shared entry:
  - is never in a private profile (C7);
  - never runs at level off (C8), because the sandbox is then the only thing between the two profiles' sessions;
  - shows "Shared with Personal" on its card (shown in the prototype).

  Sessions on it still belong to one profile each.
- **Dedicated-user hosts** (a homelab where `assistant` exists only for agents) typically run at level off: the uid
  and the host's own firewall are the boundary (user decision 3). No new flag is needed. The off warning names the
  user ("Not sandboxed: the agent can do anything assistant can on homelab"), which reads right on a dedicated user
  and stays alarming on the user's own account. The profile's sandbox defaults carry the level (Sandbox tab, profile
  card; shown in the prototype).
- **Work machines where the agent runs as the user** keep nono as today (strict or relaxed). A later private home
  per profile hardens them (§11).
- **Names** stay unique per owner: the Personal entry on devbox is `devbox-agent`. Aggregated views (notifications,
  palette) then never show two "devbox".

## 6. Setup and moving (question 3)

1. **Start fresh:** the profiles release refuses the older stores, which are deleted on deploy
   (`docs/deployment.md`). A fresh store has no profiles.
2. **The first profile is part of setup.** While none exists, the action that needs one (adding a machine,
   creating a project, a task or a session) first asks for it in a small "New profile" dialog (name, color,
   private), then carries on; Settings shows the picker with only ＋. The controller refuses those actions with
   `invalid_params` ("create a profile first") while no profile exists. With exactly one profile, `profile_id` and
   `profile_ids` may be omitted (the controller fills in the only one), and the UI looks as today.
3. **Create more profiles:** Settings, the picker's ＋ (name, color, private).
4. **Move things** with one action, offered from:
   - a project's editor ("Move to profile…");
   - a machine card ("Move to profile…" / "Share with…");
   - a session's ⋯ menu.

   The dialog previews everything that moves together and what breaks (`profiles.move` with `dry_run`), then
   sends one atomic `profiles.move {profile_id, machine_ids, project_ids, session_ids, share}`:
   - **projects** move with their tasks. A task that is also in a project staying behind blocks the move until it
     is unassigned from one;
   - **machines** change membership: replaced, or added when sharing;
   - **sessions** move when every workspace machine is in the target after the move. agentd gets `session.move`:
     the profile and policy change, and a live agent restarts and resumes (the same relaunch as a sandbox-level
     change; refused with `busy` while a run is active or the agent is starting). Their schedules follow;
   - **links and grants that would cross** (task links to sessions staying behind, contact grants between moved and
     staying sessions) are removed, and the dialog lists them first.
5. **Existing sessions** stay where they are until moved. An archived session can be moved like any other (it gets
   the new policy when it is unarchived and starts).

Moving is the owner's deliberate act and is logged with the device, like sandbox-level changes. Agents can't move
anything.

## 7. Edge cases (question 6)

| Case | Behavior |
| --- | --- |
| A session's machine moves to another profile | The session keeps its profile. Its workspace there becomes unavailable ("devbox moved to Personal") the way a removed machine's does, and agentd refuses host calls there (A8). Jobs already running on the host keep running; the session reaches them again only if the machine comes back. The move dialog lists the affected open sessions and offers to move them along or share the machine instead. |
| Deleting a profile | Allowed when it has no machines, projects, tasks or open sessions (C10). Its archived sessions are deleted with it after a confirm naming the count; its sandbox defaults and model policy go with it. Deleting the last one (empty, so nothing exists any more) returns to the fresh-install state. |
| Archived sessions | Keep their profile; listed under it with the archived filter. Unarchive works as today; the session starts with its profile's current policy. Retention is global. |
| The first profile | An ordinary profile, created during setup with the name, color and privacy the user picks: renamable, recolorable, can become private (after its accounts are listed), deletable once empty. Nothing distinguishes it from later ones. |
| Command palette | Lists sessions of every profile; a session's hint starts with its profile's name. Opening one switches to its profile. A `:profile <name>` command switches profile. |
| Deep links | `#/s/<id>` opens the session and switches to its profile (prototype). A link to a deleted session says so, as today. |
| Notifications, bell, tab title | ∗ aggregates every profile; a project scope shows that project. Cards carry their profile's chip; opening one switches profile. The tab title counts every profile. |
| Search (full text, later) | The owner's search covers the current profile, with an "All profiles" toggle. Results carry the profile chip. Nothing agent-facing searches across profiles. |
| Private terminals, templates (later) | Belong to a project, so to its profile; templates name a profile. |
| Overlapping workspaces | On a shared machine, two profiles' sessions may use one directory; the overlap warning shows as today (the owner sees both). |
| `machine:path` in the UI | Text is linked against the session's own machines (names are unique per owner anyway). |
| Keyboard | Alt-0 is the current profile's ∗, Alt-1…9 its projects. |
| A stale UI | Every write is checked server-side (§2). The answer `policy_denied` shows as a toast naming the session's profile. |

## 8. Network and hosts

Out of scope by decision 2. controlroom connects as the entry's user and doesn't configure host firewalls,
resolvers or proxies. Per-session network approvals, where wanted, come from nono's per-session proxy as today
(allowed domains in the profile's sandbox defaults). The S2 findings about daemons acting for users (nscd, the nix
daemon) remain true and are the nix-config's business. A short note in `docs/deployment.md` points there.

## 9. Push and notifications

- In-app: the panel shows every profile under ∗, tagged with a chip (a lock for private).
- Push: a private profile's payload is metadata only: "Assistant" as the title, the attention kind as the body
  ("Needs you", "Question", "Finished", "Failed"), and the session id for the deep link. Other profiles push as
  today.
- Preferences stay global. A per-profile mute is a later option.

## 10. UI behavior

The prototype implements this on the fixture build (`?profiles=3`: Work, Personal, private Assistant;
`?profiles=1`: one profile). Its screenshots are listed in the D1 report.

- **One profile: nothing changes.** There are no labels, chips or picker; the prototype's one-profile screenshots
  are byte-identical to today's (desktop and phone).
- **Rail:**
  - entries group by profile, each with a thin label (a lock when private) and a colored left edge (full color on
    the current profile);
  - each group holds the profile's ∗ ("All Work") and its projects, with no ○ Unassigned entry: the ∗ list already
    groups sessions on no task under "Not on a task";
  - clicking any entry enters its profile;
  - ＋ (new project), machine status and ⚙ stay at the bottom. Machine status covers every profile's machines.
- **Scoping:** the session list (titled with the profile on ∗), the board, filters, grouping by machine, the
  new-session dialog and new tasks use the current profile's data only.
- **New session:** the first field is Profile (the current one; changing it resets the workspaces). Machines and
  tasks are the profile's, and the default sandbox level counts the profile's default. Model choices need nothing
  in the dialog: the session's model picker lists only what pi offers, which agentd already limited (A1).
- **Session header:** the second line starts with the profile chip (color, name, lock), only with several profiles.
  The chip leads to the profile's ∗.
- **Settings:**
  - Machines, Models and Sandbox start with the profile picker (one pill per profile, ＋, and the selected
    profile's ⋯ menu: Rename, Color, Private, Delete);
  - Machines lists the profile's entries ("Shared with" chips). SSH keys stay below, global;
  - the agent daemon's session settings (idle stop, retention) move to General;
  - Models has "Accounts in Work: Any / Only the checked ones" and a checkbox per account; unused accounts are
    dimmed, and Defaults become "Defaults in Work";
  - Sandbox has the profile's own card (its level) above its machines and projects, and its contact grants;
  - Notifications (with Devices) and General stay global.
- **Phone:** the drawer groups the same way; the header shows the profile's name on ∗ and a lock when private; the
  bell counts every profile.
- **Copy:** "All Work", "Accounts in Work", "Shared with", "Work default (strict)". Chips and pickers have full
  aria-labels ("Profile Assistant (private)"); the lock carries "private".

## 11. Private home on machines where the agent runs as the user (question 7)

On a machine entry that is the user's own account (a work laptop where no other user can be created), shells, jobs
and the fs worker of a session could get a private home per profile:

- `HOME` and `XDG_{CONFIG,CACHE,DATA,STATE}_HOME` under `~/.local/share/controlroom-homes/<profile>`, granted
  read-write;
- the real home stays ungranted, except the workspaces and the profile's grants.

`relaxed` would then read the profile home's config (its own git identity) instead of the real one.

Findings and constraints:

- The directory must not be under `~/.local/share/controlroom`: hosts drop grants that overlap the daemon's own
  state (§11.6).
- It contains none of nono's always-denied paths, so granting it works.
- nono itself runs with the real `HOME`; whether its home-relative deny groups follow the redirected `HOME` needs a
  spike.
- Caches per profile cost disk; a first-use seed (copying the user's git identity) is a UI choice.
- It is a boundary between the agent and the user's real home, and between two profiles on one uid **only while the
  sandbox is on**.

**Recommendation: later (P3), after a nono spike.** Dedicated-user hosts don't need it, and work machines are
usually single-profile. It would make shared entries safer, so if the user wants shared entries early it moves up
with them.

## 12. Phased build plan

| Phase | Lane | Touches | Depends on | Done when | Risk |
| --- | --- | --- | --- | --- | --- |
| **P0 Contracts** | protocol | `crates/protocol` (§4 types, methods, `policy_denied`, push payload, the sandbox-defaults rename), `docs/protocol.md`, generated TS; mechanical fixture and UI renames | — | Types merged, TS drift check green, the UI builds | Low. The rename touches many UI files mechanically. |
| **P1 Foundations** (parallel) | **a. controller** | `crates/controller` store (schema bump with no upgrade, profiles repo, `machine_profiles`, project and task profile), RPC (`profiles.*`, `profiles.move`, C1–C13), `Fill::Profile` for `session.create`, `profiles.report` and machine `profile_ids` to agentd, peer directory filter, push redaction | P0 | A fresh store has no profiles and an older one is refused; RPC tests per refusal; push redaction test | Medium. `profiles.move` is the one multi-entity transaction. |
| | **b. agentd** | `crates/sandbox` (port `ProviderAllowList` from the spike, re-applied on current main, not merged), `crates/agentd` (launch filter, server sandbox always on, env stripping, credential manifest, `profiles.report`, `session.move` restart), `crates/runtime` (`sessions.profile_id` and `profile_policies` with a schema bump and no upgrade, `set_option` check, host-call profile check), `nix/rust.nix` (port the `agentd-pi-model-policy` check) | P0 | Unit tests; real-pi check green (`--list-models` without unlisted, `--provider` refused, no call to the paid mock) | Medium. Always-on server sandbox changes level-off behavior (decision 1, §14); hosts that relied on off-mode sign-ins need accounts. |
| | **c. UI** | `web/` (rewrite the prototype's behavior cleanly against the generated types: profile state from the feed, rail groups, scoping, chip, new-session profile, Settings picker and per-profile tabs, server settings to General, notification tags, palette and deep links, move dialog, `policy_denied` toasts) | P0 (fixture first) | `pnpm check`/`test`; Playwright: one profile pixel-identical to none, a three-profile walk-through, phone | Low–medium. Scoping must be one derived context, not per-component filters (the prototype's `App.ctx` / `App.all` split). |
| **P2 End to end** | e2e + docs | `crates/e2e` (two profiles over two host daemons: unlisted model refused at launch and in `set_option`; workspace on another profile's machine refused; peers absent; machine move makes a workspace unavailable and host calls refused; push redacted), `docs/architecture.html` (§2 "separate instances" note, §5, §10, §11.6, §12, §13), `docs/deployment.md` (network is nix-config's) | P1 a–c | e2e green, docs reviewed | Low. |
| | R3 hooks | Subagent lane: A5; mailbox lane: A10, C5 | P1 b | Each lane's acceptance includes a cross-profile refusal | Coordination: these features aren't built yet. |
| **P3 Later** | — | Private home per profile (§11, spike first); usage per profile and account; search scoping; per-profile mute; per-profile transcript encryption | P2 | — | The nono spike may show the redirect is impractical. |

Ordering: P0 first (one short lane). Then P1 a, b and c in parallel: the UI works on the fixture until a and b land.
Then P2. Version bumps: P1 b and P2 change daemons (agentd), so each bumps the workspace version at merge.

## 13. What this keeps from the S2 spike

| S2 decision | Status here |
| --- | --- |
| A profile is a trust domain (uid, network, models, circle) | Kept as a tenant without the network part. |
| A machine entry belongs to exactly one profile | Kept, plus the explicit shared flag (§5). |
| Hard boundaries: uid, nftables, agentd, controller | uid, agentd and controller kept; nftables is nix-config's. |
| nix-config host module, egress proxy, signed approval relay, no DNS for proxy-only profiles, hold-and-approve | Dropped (decision 2). The spike's VM test and proxy stay on `spike/profiles` as reference. |
| Model allow-list in agentd (`ProviderAllowList`) | Kept and ported (A1, A4, A6). Level-off refusal replaced by the always-on server sandbox (A2). |
| New level `profile` (no nono) on profile hosts | Replaced by: level off on dedicated-user entries, with an honest warning (§5). |
| Projects optionally bound to a profile | Replaced: projects always belong to one profile. |
| Metadata-only push for private profiles; no cross-profile contact | Kept. |

## 14. Decisions for the user (all accepted; see the header)

1. **Server sandbox always on** (level off then unsandboxes only the hosts)? Recommended. The alternative keeps
   today's off and refuses it in profiles with a list.
2. **Shared machine entries** between non-private profiles, explicit and never at level off? Recommended. The
   alternative is one profile per entry, no exceptions.
3. **Machine names unique per owner** (`devbox-agent`)? Recommended. The alternative is unique per profile (both
   "devbox"; needs a machines-table rebuild and per-profile name resolution).
4. **Private profile** = metadata-only push, no shared machines and an explicit account list. Anything to add or
   drop?
5. **Moving a session between profiles** (explicit, restarts a live agent)? Recommended. The alternative: sessions
   never move.
6. **No ○ Unassigned in the rail with several profiles**? Recommended. The alternative is one ○ per profile that has
   projects.
7. **Deleting a profile** requires it empty except archived sessions, which are deleted with it?
8. **Private home per profile** (§11) later, after a spike, rather than in the first build?
9. **Rename the sandbox's "profiles"** to "defaults" (`sandbox.defaults`, `SandboxDefaults`) in P0?
10. **SSH keys global, server session settings moved to General**?
11. **Notifications under ∗ aggregate every profile**, while a project scope stays scoped?
