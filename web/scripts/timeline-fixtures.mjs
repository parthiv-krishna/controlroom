#!/usr/bin/env node
// Generates web/fixtures/timelines.json, the fixture transport's session timelines, from the
// PiDriver goldens (crates/drivers/tests/golden/pi/*.json: the ops pi's recordings fold into) and
// the recordings themselves (fixtures/pi-rpc/1.2.0/*.jsonl: the raw frames).
//
// Each fixture session gets "chapters": recordings laid end to end, with seqs, ids and times
// shifted so they read as one session. Backlog chapters are materialized into items (what
// `session.subscribe` sends first); live chapters become scripted `session.events` steps, one per
// folded record, played after the session is subscribed.
//
// Kinds pi never produces (todo, subagent, job, session_message, a withdrawn dialog, a child
// actor, an unknown kind) are daemon-sourced; they are written by hand below (`synthetic`), as are
// run triggers and user message origins the runtime fills in (devices, schedules, injections). The recordings ran pi's local tools; the
// remote tools name files `machine:path`, so a tool's relative `path` gets the session's primary
// machine (`notes.txt` → `devbox:notes.txt`).
//
// Run from web/: node scripts/timeline-fixtures.mjs
import { readFileSync, writeFileSync } from 'node:fs';

const root = new URL('../../', import.meta.url);
const GOLDEN = new URL('crates/drivers/tests/golden/pi/', root);
const RECORDINGS = new URL('fixtures/pi-rpc/1.2.0/', root);
const OUT = new URL('web/fixtures/timelines.json', root);
/** Parses JSON, naming its source when it is malformed (the generator stops then). */
function parseJson(text, source) {
  try {
    return JSON.parse(text);
  } catch (e) {
    throw new Error(`${source}: ${e.message}`, { cause: e });
  }
}

const readJson = (url) => parseJson(readFileSync(url, 'utf8'), url.pathname);
const fixture = readJson(new URL('web/fixtures/controlroom.json', root));

/** The agent daemon: every session's `PeerRef.daemon_id`. */
const AGENTD = '01JAA000000000000000000001';
const sid = (n) => `01JAS0000000000000000000${String(n).padStart(2, '0')}`;
const LAPTOP = { device_id: 'device-laptop', label: 'Laptop (Firefox on Linux)' };
const PHONE = { device_id: 'device-phone', label: 'Phone (Safari on iPhone)' };

/** Gap between chapters, in ms of fixture time. */
const CHAPTER_GAP = 7 * 60_000;
/** Raw frame text is capped (the recordings carry the whole system prompt). */
const RAW_CAP = 600;

// ------------------------------------------------------------------------------------ sources

const golden = (name) => readJson(new URL(`${name}.json`, GOLDEN));
const recording = (name) =>
  readFileSync(new URL(`${name}.jsonl`, RECORDINGS), 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((l, i) => parseJson(l, `${name}.jsonl:${i + 1}`));

// ----------------------------------------------------------------------------------- rewriting

/** A tool call's relative `path` (title and input) as `machine:path`. */
function remotePath(it, machine) {
  let input;
  try {
    input = JSON.parse(it.input ?? '');
  } catch {
    return it;
  }
  const p = input?.path;
  if (typeof p !== 'string' || p.includes(':')) return it;
  const remote = `${machine}:${p}`;
  return { ...it, title: it.title.split(p).join(remote), input: JSON.stringify({ ...input, path: remote }, null, 2) };
}

/** The origin of a message that starts a run with `trigger` (the runtime records both together). */
function originOf(trigger) {
  if (!trigger) return { kind: 'user', device: null };
  if (trigger.kind === 'inject') return { kind: 'inject', origin: trigger.origin, label: trigger.label, source: null };
  return trigger;
}

/**
 * The origins of a recording's user messages, by item id: `c.origins` in order of appearance, else
 * the first from the chapter's trigger and later ones as steers.
 */
function userOrigins(g, c) {
  const ids = [];
  for (const f of g.folded)
    for (const o of f.ops) if (o.op === 'upsert' && o.item.kind === 'message' && o.item.role === 'user' && !ids.includes(o.item.id)) ids.push(o.item.id);
  const fallback = (i) => (i === 0 ? originOf(c.trigger) : { kind: 'steer', device: null });
  return new Map(ids.map((id, i) => [id, c.origins ? (c.origins[i] ?? null) : fallback(i)]));
}

/** Shifts one chapter into its place in the session: seq offset, id prefix, time offset, origins. */
function shifter({ seq, prefix, dt, trigger, origins, machine }) {
  const id = (x) => (x == null ? x : prefix + x);
  const item = (it) => {
    let out = { ...it, id: id(it.id), seq_from: it.seq_from + seq, seq_to: it.seq_to + seq, ts: it.ts + dt };
    if (it.kind === 'tool_call') out = remotePath({ ...out, tool_call_id: id(it.tool_call_id), parent_tool_call_id: id(it.parent_tool_call_id) }, machine);
    if (it.kind === 'run' && trigger) out.trigger = trigger;
    if (it.kind === 'message' && it.role === 'user') out.origin = origins.get(it.id) ?? null;
    if (it.kind === 'request') {
      const r = it.request;
      out.request = { ...r, id: id(r.id), created_at: r.created_at + dt, answer: r.answer && { ...r.answer, at: r.answer.at + dt } };
    }
    return out;
  };
  const op = (o) => {
    if (o.op === 'upsert') return { op: 'upsert', item: item(o.item) };
    return { ...o, id: id(o.id), seq_from: o.seq_from + seq, seq_to: o.seq_to + seq };
  };
  const STREAM = { in: 'to_agent', out: 'from_agent', stderr: 'stderr' };
  const frame = (r) => {
    const shifted = { ...r, seq: r.seq + seq, ts: r.ts + dt };
    const text = STREAM[r.dir] ? r.raw : JSON.stringify(shifted);
    return { seq: shifted.seq, ts: shifted.ts, stream: STREAM[r.dir] ?? 'meta', text: text.length > RAW_CAP ? text.slice(0, RAW_CAP) + '…' : text };
  };
  return { item, op, frame };
}

/** Applies ops the way the daemon's materializer does. */
function materialize(items, ops) {
  for (const o of ops) {
    if (o.op === 'upsert') {
      items.set(o.item.id, o.item);
      continue;
    }
    const it = items.get(o.id);
    if (!it) continue;
    const next = { ...it, seq_to: Math.max(it.seq_to, o.seq_to) };
    if (o.op === 'end') next.streaming = false;
    else if (it.kind === 'message') {
      const content = [...it.content];
      const last = content.at(-1);
      if (last?.kind === 'text') content[content.length - 1] = { kind: 'text', text: last.text + o.text };
      else content.push({ kind: 'text', text: o.text });
      next.content = content;
    } else if (it.kind === 'thinking') next.text = it.text + o.text;
    else if (it.kind === 'tool_call') {
      const out = it.output;
      next.output = out?.kind === 'text' || out?.kind === 'terminal' ? { ...out, text: out.text + o.text } : { kind: 'text', text: o.text };
    }
    items.set(o.id, next);
  }
}

// ------------------------------------------------------------------------------------ sessions

/**
 * Builds one session's timeline from its chapters. A chapter is a recording name, or
 * `{ name, live?, trigger?, origins? }` (see `userOrigins`), or `{ synthetic: (seq, ts) => items }`. `withRaw: false` leaves out the
 * raw frames (the long timeline, to keep the fixture small).
 */
function session(key, machine, chapters, endTs, withRaw = true) {
  const items = new Map();
  const raw = [];
  const live = [];
  let seqBase = 0;
  const count = chapters.length;
  chapters.forEach((spec, n) => {
    const c = typeof spec === 'string' ? { name: spec } : spec;
    const startTs = endTs - (count - n) * CHAPTER_GAP;
    if (c.synthetic) {
      const made = c.synthetic(seqBase, startTs);
      for (const it of made) items.set(it.id, it);
      seqBase = Math.max(seqBase, ...made.map((it) => it.seq_to)) + 1;
      return;
    }
    const g = golden(c.name);
    const records = recording(c.name);
    const origins = userOrigins(g, c);
    const shift = shifter({ seq: seqBase, prefix: n ? `c${n}.` : '', dt: startTs - records[0].ts, trigger: c.trigger, origins, machine });
    const frames = withRaw ? records.map(shift.frame) : [];
    if (!c.live) {
      materialize(items, g.folded.flatMap((f) => f.ops.map(shift.op)));
      raw.push(...frames);
    } else {
      let sent = seqBase;
      let lastTs = startTs;
      for (const f of g.folded) {
        const head = f.seq + seqBase;
        const stepFrames = frames.filter((x) => x.seq > sent && x.seq <= head);
        const ts = stepFrames.at(-1)?.ts ?? lastTs;
        live.push({ delay_ms: Math.min(600, Math.max(40, ts - lastTs)), head_seq: head, ops: f.ops.map(shift.op), raw: stepFrames });
        sent = head;
        lastTs = ts;
      }
      const rest = frames.filter((x) => x.seq > sent);
      if (rest.length) live.push({ delay_ms: 200, head_seq: rest.at(-1).seq, ops: [], raw: rest });
    }
    seqBase += records.at(-1).seq;
  });
  const backlog = [...items.values()].sort((a, b) => a.seq_from - b.seq_from);
  const head = Math.max(0, ...backlog.map((it) => it.seq_to), ...raw.map((f) => f.seq));
  return [key, { head_seq: head, items: backlog, raw, live }];
}

// -------------------------------------------------------------------------- hand-written items

const base = (id, seq_from, seq_to, ts) => ({ id, seq_from, seq_to, actor: 'root', ts, streaming: false });
const userMsg = (id, seq, ts, text, origin) => ({ ...base(id, seq, seq, ts), kind: 'message', role: 'user', content: [{ kind: 'text', text }], origin });
const agentMsg = (id, seq, ts, text) => ({ ...base(id, seq, seq, ts), kind: 'message', role: 'agent', content: [{ kind: 'text', text }], origin: null });
const inject = (origin, label, source = null) => ({ kind: 'inject', origin, label, source });
const run = (id, from, to, ts, trigger, outcome = 'completed') => ({ ...base(id, from, to, ts), kind: 'run', trigger, state: 'settled', outcome, error: null });

/** A dialog withdrawn when its run settled (pi resolved it itself); no recording shows one. */
const withdrawnDialog = (s, ts) => [
  userMsg('w.user', s + 1, ts, 'Deploy the pipeline, ask me for the region.', { kind: 'user', device: PHONE }),
  run('w.run', s + 2, s + 9, ts + 400, { kind: 'user', device: PHONE }, 'cancelled'),
  {
    ...base('w.dialog', s + 4, s + 9, ts + 900),
    kind: 'request',
    request: {
      id: 'w.dialog', kind: 'dialog', blocking: true, state: 'withdrawn', title: 'Deploy to which region?', detail: null, body: null,
      options: [{ id: '0', label: 'eu-west', style: 'neutral' }, { id: '1', label: 'us-east', style: 'neutral' }],
      questions: [], answer: null, origin_child: null, created_at: ts + 900,
    },
  },
];

/** The coordinator's daemon-sourced items: subagents, the todo list, mailbox messages. */
const coordinator = (s, ts) => {
  const child = (n, name) => ({ daemon_id: AGENTD, session_id: sid(n), name });
  return [
    userMsg('k.user', s + 1, ts, 'Sweep the context sizes: spawn one subagent per range and compare the results.', { kind: 'user', device: LAPTOP }),
    run('k.run', s + 2, s + 30, ts + 300, { kind: 'user', device: LAPTOP }),
    {
      ...base('k.todo', s + 3, s + 3, ts + 600),
      kind: 'todo',
      items: [
        { id: '01JAT000000000000000000901', text: 'Sweep 8k–32k', status: 'in_progress', reason: null, updated_by: { kind: 'agent' }, updated_at: ts + 600 },
        { id: '01JAT000000000000000000902', text: 'Sweep 64k–128k', status: 'blocked', reason: 'waiting for approval', updated_by: { kind: 'agent' }, updated_at: ts + 600 },
        { id: '01JAT000000000000000000903', text: 'Compare and write up', status: 'pending', reason: null, updated_by: { kind: 'user', device: LAPTOP }, updated_at: ts + 600 },
      ],
    },
    { ...base('k.sub13', s + 4, s + 4, ts + 900), kind: 'subagent', child: child(13, 'sweep: 8k–32k'), title: 'sweep: 8k–32k', agent: 'pi', state: 'working', outcome: null, usage: { input: 18200, output: 2100, cache_read: 9000, cache_write: 0 }, result: null },
    { ...base('k.sub12', s + 5, s + 5, ts + 1200), kind: 'subagent', child: child(12, 'sweep: 64k–128k'), title: 'sweep: 64k–128k', agent: 'pi', state: 'awaiting_permission', outcome: null, usage: null, result: null },
    {
      ...base('k.notice12', s + 6, s + 6, ts + 1500),
      actor: sid(12),
      kind: 'notice',
      level: 'warning',
      text: 'Needs approval to run `bench --ctx 131072`.',
    },
    {
      ...base('k.mail1', s + 7, s + 7, ts + 1800),
      kind: 'session_message', message_id: '01JAX000000000000000000901', direction: 'sent',
      peer: { daemon_id: AGENTD, session_id: sid(9), name: 'migrate UI to Svelte 5 runes' },
      text: 'Is the **benchmark page** ready for the new numbers?', reply_to: null, expects_reply: true, outcome: 'delivered', late: false,
    },
    {
      ...base('k.mail2', s + 8, s + 8, ts + 2400),
      kind: 'session_message', message_id: '01JAX000000000000000000902', direction: 'received',
      peer: { daemon_id: AGENTD, session_id: sid(9), name: 'migrate UI to Svelte 5 runes' },
      text: 'Yes, push them to `bench/results.json`.', reply_to: '01JAX000000000000000000901', expects_reply: false, outcome: 'delivered', late: false,
    },
    { ...base('k.future', s + 9, s + 9, ts + 2600), kind: 'unknown' },
    // Input steered into the run: a workspace the user added, a peer's message, a child's
    // report, the user's send-now.
    userMsg(
      'k.workspace', s + 10, ts + 2700,
      '[controlroom] The user added the workspace buildbox:~/src/bench (shell "buildbox"); <workspaces> lists it from your next turn.',
      inject('workspace', 'workspaces', '01JAW000000000000000000013'),
    ),
    userMsg(
      'k.peer', s + 11, ts + 2750,
      '[controlroom] Message from session "migrate UI to Svelte 5 runes": heads-up, the benchmark page caches results for 10 minutes.',
      inject('message', 'message: migrate UI to Svelte 5 runes', sid(9)),
    ),
    userMsg(
      'k.child', s + 12, ts + 2780,
      '[controlroom] Subagent "sweep: 8k–32k" reports: 8k and 16k are done, 32k is running.',
      inject('child', 'subagent: sweep: 8k–32k', sid(13)),
    ),
    userMsg('k.steer', s + 13, ts + 2790, 'Skip 128k if it needs more than an hour.', { kind: 'steer', device: LAPTOP }),
    agentMsg('k.reply', s + 14, ts + 2800, 'Both sweeps are running. The 64k–128k range is waiting for your approval.'),
  ];
};

/**
 * Jobs on buildbox: one whose exit wakes the idle agent (one line, folded into the job's row), one
 * whose wake in the production shape (written before messages carried their origin) is steered
 * into the user's run and folds by the job its text names, and one still running.
 */
const jobs = (s, ts) => {
  const ws = fixture.snapshot.sessions.find((x) => x.id === sid(3)).workspaces[0];
  const job = (id, name, command, status, exit, lines, ended) => ({
    id, name, command, cwd: '/home/alice/src/controlroom', machine_id: ws.machine_id, workspace_id: ws.id,
    status, exit, signal: status === 'killed' ? 'SIGKILL' : null,
    started_at: ts, ended_at: ended, timeout_s: null, log_path: `/home/alice/src/controlroom/.controlroom/jobs/${id}.log`, lines, follow: null,
  });
  const PAAS = '01JAJ000000000000000000903';
  return [
    { ...base('j.tests', s + 1, s + 4, ts), kind: 'job', job: job('01JAJ000000000000000000901', 'tests', 'pnpm e2e --repeat-each 20', 'failed', 1, 2311, ts + 240_000) },
    userMsg(
      'j.wakeMsg', s + 5, ts + 240_500,
      '[controlroom] Job 01JAJ000000000000000000901 "tests" exited with code 1 after 4m 00s; 2311 lines in buildbox:/home/alice/src/controlroom/.controlroom/jobs/01JAJ000000000000000000901.log.',
      inject('job', 'job: tests (exit 1)', '01JAJ000000000000000000901'),
    ),
    run('j.wake', s + 6, s + 9, ts + 241_000, { kind: 'inject', origin: 'job', label: 'job: tests (exit 1)' }),
    agentMsg('j.reply', s + 7, ts + 242_000, 'Two failures, both in `e2e/queue.spec.ts`: the send-now race again.'),
    { ...base('j.lint', s + 10, s + 10, ts + 250_000), kind: 'job', job: job('01JAJ000000000000000000902', 'typecheck', 'pnpm check --watch', 'running', null, 48, null) },
    { ...base('j.paas', s + 11, s + 11, ts + 260_000), kind: 'job', job: job(PAAS, 'Final PaaS validation', './validate.sh --final', 'failed', 1, 220, ts + 361_000) },
    userMsg('j.user', s + 12, ts + 300_000, 'Meanwhile, tidy the release notes.', { kind: 'user', device: LAPTOP }),
    run('j.userRun', s + 13, s + 17, ts + 300_500, { kind: 'user', device: LAPTOP }),
    agentMsg('j.notes', s + 14, ts + 330_000, 'Tidying the release notes.'),
    userMsg(
      'j.paasWake', s + 15, ts + 361_500,
      `[controlroom] Background job update:\n- job ${PAAS} \`Final PaaS validation\` exited with code 1 after 1m 41s; 220 lines in buildbox:/home/alice/src/controlroom/.controlroom/jobs/${PAAS}.log. Followed lines not reported yet: lines 81, 115, 186, 188-189, 216, 218. Logs are not in your context: read only what you need, e.g. \`read\` buildbox:/home/alice/src/controlroom/.controlroom/jobs/${PAAS}.log with an offset, \`grep\` in it, or \`tail -n 50 /home/alice/src/controlroom/.controlroom/jobs/${PAAS}.log\` / \`sed -n 'a,bp' /home/alice/src/controlroom/.controlroom/jobs/${PAAS}.log\` in a shell on buildbox.`,
      null,
    ),
    agentMsg('j.paasReply', s + 16, ts + 362_000, 'The PaaS validation failed at line 81: a missing route. Fixing it next.'),
  ];
};

/** File requests on devbox answered with "Always allow" and "Never" (made standing on the machine), then the sandbox turned off. */
const fileDecisions = (s, ts) => {
  const devbox = fixture.snapshot.machines.find((m) => m.name === 'devbox').id;
  const request = (id, seq, at, path, access, option, label) => ({
    ...base(id, seq, seq + 1, at),
    kind: 'request',
    request: {
      id, kind: 'sandbox', blocking: true, state: 'answered', title: `${access === 'read' ? 'Read' : 'Write'} ${path}?`, detail: null,
      body: { kind: 'path', path, access, in_workspace: false, machine_id: devbox, machine: 'devbox' },
      options: [
        { id: 'allow-session', label: 'Allow for this session', style: 'allow_always' },
        { id: 'allow-always', label: 'Always allow (devbox)', style: 'allow_always' },
        { id: 'deny', label: 'Deny', style: 'deny' },
        { id: 'never', label: 'Never (devbox)', style: 'deny' },
      ],
      questions: [],
      answer: { response: { kind: 'choice', option_id: option }, label, by: LAPTOP, at: at + 4000 },
      origin_child: null, created_at: at,
    },
  });
  return [
    userMsg('f.user', s + 1, ts, 'Build the deploy bundle with nix.', { kind: 'user', device: LAPTOP }),
    run('f.run', s + 2, s + 9, ts + 300, { kind: 'user', device: LAPTOP }),
    request('f.always', s + 3, ts + 900, '/home/alice/.cache/nix/fetcher-cache-v4.sqlite', 'read_write', 'allow-always', 'Always allow (devbox)'),
    request('f.never', s + 5, ts + 1800, '/home/alice/.bash_history', 'read', 'never', 'Never (devbox)'),
    agentMsg('f.reply', s + 8, ts + 2600, 'The bundle is built.'),
    // Then the user turned the session's sandbox off.
    { ...base('f.sandbox', s + 10, s + 10, ts + 3000), kind: 'session_info', title: null, model: null, thinking: null, sandbox: 'off' },
  ];
};

/**
 * After the sandbox went off: notices that ride along into the user's next run (the sandbox level,
 * todo edits), a late tool result and an answered question that wake the agent.
 */
const wakes = (s, ts) => {
  const late = inject('tool_late', 'late result: bash `./deploy.sh --dry-run`', 'call_d41');
  const answered = inject('answer', 'answer: Which environment?', 'n.question');
  return [
    userMsg('n.user', s + 1, ts, 'Now run the deploy dry-run.', { kind: 'queue', device: LAPTOP }),
    run('n.run', s + 2, s + 6, ts + 300, { kind: 'queue', device: LAPTOP }),
    userMsg(
      'n.sandbox', s + 3, ts + 400,
      '[controlroom] The user set the sandbox level to off (your shells, jobs and file tools on the hosts run unsandboxed). Your shells restart (their variables are lost); running jobs keep the old level.',
      inject('workspace', 'sandbox level'),
    ),
    userMsg('n.todo', s + 4, ts + 450, '[controlroom] The user changed your todo list:\n- added "check the rollback script"', inject('todo', 'todo edits')),
    agentMsg('n.reply', s + 5, ts + 2000, 'The dry-run stalled: devbox stopped answering.'),
    userMsg(
      'n.late', s + 7, ts + 60_000,
      '[controlroom] devbox is back. Your call call_d41 (bash `./deploy.sh --dry-run`), which failed while devbox was unreachable, ran once and ended with exit 0:\ndry-run ok',
      late,
    ),
    run('n.lateRun', s + 8, s + 10, ts + 60_100, { kind: 'inject', origin: 'tool_late', label: late.label }),
    {
      ...base('n.question', s + 9, s + 12, ts + 60_500),
      kind: 'request',
      request: {
        id: 'n.question', kind: 'question', blocking: false, state: 'answered', title: 'Which environment?', detail: null, body: null, options: [],
        questions: [{ id: 'q1', text: 'Which environment?', options: [{ label: 'staging', description: null }, { label: 'production', description: null }], multi: false, free_text: true }],
        answer: { response: { kind: 'answers', answers: [{ question_id: 'q1', selected: ['staging'], text: null }] }, label: 'staging', by: PHONE, at: ts + 90_000 },
        origin_child: null, created_at: ts + 60_500,
      },
    },
    userMsg(
      'n.answer', s + 13, ts + 90_100,
      '[controlroom] The user answered your question n.question:\n- Which environment?\n  → staging',
      answered,
    ),
    run('n.answerRun', s + 14, s + 16, ts + 90_200, { kind: 'inject', origin: 'answer', label: answered.label }),
    agentMsg('n.answerReply', s + 15, ts + 91_000, 'Deploying to staging.'),
  ];
};

// --------------------------------------------------------------------------------------- build

const sessions = new Map(fixture.snapshot.sessions.map((x) => [x.id, x]));
const machineNames = new Map(fixture.snapshot.machines.map((m) => [m.id, m.name]));
const build = (n, chapters, withRaw) => {
  const s = sessions.get(sid(n));
  if (!s) throw new Error(`unknown fixture session ${sid(n)}`);
  return session(s.id, machineNames.get(s.workspaces[0].machine_id), chapters, s.updated_at, withRaw);
};

const LONG = ['plain-text', 'tool-calls', 'thinking', 'steer', 'nested-tool', 'retry-success', 'parallel-tools'];
const timelines = Object.fromEntries([
  // Streams a permission dialog live after a backlog (answer it from the timeline).
  // Its steer is a send-now from the laptop.
  build(4, [
    { name: 'plain-text', trigger: { kind: 'user', device: LAPTOP } },
    { name: 'steer', origins: [{ kind: 'user', device: LAPTOP }, { kind: 'steer', device: LAPTOP }] },
    'tool-calls',
    { name: 'permission-dialog', live: true },
  ]),
  // A subagent streaming thinking, text and nested tools (also its parent's live tail).
  build(13, ['plain-text', { name: 'thinking', live: true }, { name: 'nested-tool', live: true }]),
  // Its first message is from before origins existed (`origin: null`).
  build(9, [
    { name: 'nested-tool', origins: [] },
    'parallel-tools',
    'model-thinking',
    'error-stop',
    { name: 'queue-when-idle', origins: [{ kind: 'user', device: PHONE }, { kind: 'steer', device: PHONE }, { kind: 'queue', device: PHONE }] },
  ]),
  // Dialogs answered, expired and withdrawn; then the agent was killed.
  build(8, ['extension-dialogs', 'extension-dialog-timeout', 'abort-dialog', { synthetic: withdrawnDialog }, 'crash-sigkill']),
  build(7, ['retry-success', { name: 'retry-exhausted', trigger: { kind: 'auto_continue' } }]),
  build(3, [{ name: 'compact', origins: [{ kind: 'user', device: null }, { kind: 'queue', device: LAPTOP }] }, { synthetic: jobs }, 'length-stop']),
  build(2, [
    'abort-text',
    'abort-tool',
    { name: 'abort-then-prompt', origins: [{ kind: 'user', device: null }, { kind: 'user', device: LAPTOP }] },
    { name: 'follow-up', trigger: { kind: 'queue', device: LAPTOP }, origins: Array(3).fill({ kind: 'queue', device: LAPTOP }) },
    { name: 'steer-during-tool', origins: [{ kind: 'user', device: null }, { kind: 'steer', device: PHONE }] },
  ]),
  build(11, [{ synthetic: coordinator }, 'plain-text']),
  build(18, [{ name: 'plain-text', trigger: { kind: 'schedule', schedule_id: '01JAY000000000000000000001', name: 'Inbox triage' } }, 'sigterm-idle']),
  // File requests answered "Always allow" and "Never".
  // Then notices, a late tool result and an answer (one of each injection the runtime writes).
  build(15, [{ synthetic: fileDecisions }, { synthetic: wakes }]),
  // A long timeline (windowing).
  build(6, Array.from({ length: 40 }, (_, i) => LONG[i % LONG.length]), false),
]);

writeFileSync(OUT, JSON.stringify(timelines) + '\n');
const sizes = Object.entries(timelines).map(([k, t]) => `${k.slice(-2)}: ${t.items.length} items, ${t.raw.length} raw, ${t.live.length} live`);
console.log(`wrote ${OUT.pathname}\n${sizes.join('\n')}`);
