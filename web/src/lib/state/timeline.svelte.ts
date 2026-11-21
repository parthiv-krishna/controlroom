// The session-stream store: one session's timeline, built from `session.events` ops.
//
// - Each item lives in its own cell (`$state.raw`), so a streamed token replaces exactly one item
//   and re-renders exactly one component. Ordering and nesting change only when an item is
//   inserted, moved or trimmed.
// - Items are ordered by their latest `seq_from`, then by arrival; an upsert with a new
//   `seq_from` moves the item (a queued steer delivered later).
// - Ops are idempotent across resubscribes: `append_text`/`end` at or before the head already
//   applied are skipped, and an upsert older than the item it would replace is ignored.
// - Memory is bounded: at most MAX_ITEMS items (the oldest are trimmed) and MAX_RAW raw frames.
// - The reading view stays quiet: retries fold into their run, a notice repeated verbatim (an
//   extension's "ready" at each agent start) shows once with a count, and a session-info item
//   shows only the values that changed (the first ones are what the header already shows); a new
//   sandbox level (the user changed it) always shows.
// - Text the system wrote for the agent (a schedule's prompt, a job wake, a notice) is an event: a
//   user message with such an origin shows as one line naming its source; the run it started then
//   shows only when it didn't end normally. A message without an origin (from before messages
//   carried one) takes it from the run it started, else from its "[controlroom]" text; a job wake
//   names its job either way, so it folds into the job's row like any other. A todo item knows the
//   list before it, to show what changed.
import type { MessageOrigin, RawFrame, Request, RunTrigger, SandboxLevel, Todo, TimelineItem, TimelineOp, ToolOutput } from '../protocol';

/** Items kept per timeline; older ones are trimmed (the protocol has no paging yet). */
export const MAX_ITEMS = 5000;
/** Raw frames kept per timeline. */
export const MAX_RAW = 2000;

/** The item of one kind (the props of its component). */
export type ItemOf<K extends TimelineItem['kind']> = Extract<TimelineItem, { kind: K }>;

/** One item's reactive state. */
export class ItemCell {
  item = $state.raw() as TimelineItem;
  /** The item's id (stable; reading it doesn't subscribe to the item). */
  readonly id: string;
  /** Breaks `seq_from` ties: renewed when the item moves. */
  arrival: number;

  constructor(item: TimelineItem, arrival: number) {
    this.id = item.id;
    this.item = item;
    this.arrival = arrival;
  }
}

const before = (a: ItemCell, seqFrom: number, arrival: number): boolean =>
  a.item.seq_from < seqFrom || (a.item.seq_from === seqFrom && a.arrival < arrival);

/**
 * Items kept but not shown in the reading view: usage (the header's statusline has the session's
 * totals; the raw view still has each one) and kinds this UI doesn't know (their records are in the
 * raw view).
 */
export const hiddenInTimeline = (item: TimelineItem): boolean => item.kind === 'usage' || item.kind === 'unknown';

/** An event's source: what a message the system wrote for the agent came from. */
export type EventOrigin = Extract<MessageOrigin, { kind: 'schedule' | 'inject' | 'auto_continue' }>;

/** A user message's origin when the system wrote it (not the user), else null. */
export function eventOrigin(origin: MessageOrigin | null): EventOrigin | null {
  return origin && (origin.kind === 'schedule' || origin.kind === 'inject' || origin.kind === 'auto_continue') ? origin : null;
}

/** A job id in a wake's text (`[controlroom] Job 01J… "build" exited …`, or the older `- job 01J… \`build\` exited …`). */
const JOB_IN_WAKE = /\bjob ([0-9A-HJKMNP-TV-Z]{26})\b/gi;
/** A wake's first line, either shape. */
const WAKE_HEAD = /^\[controlroom\] (?:Background job updates?:|Jobs?\b)/;

/**
 * The origin a user message without one implies by its text: the system's messages start with
 * "[controlroom]", and a job wake names its jobs (one job is its source). Null for anything else.
 */
export function textOrigin(item: ItemOf<'message'>): EventOrigin | null {
  const text = item.content.map((p) => (p.kind === 'text' ? p.text : '')).join('');
  if (!text.startsWith('[controlroom]')) return null;
  if (WAKE_HEAD.test(text)) {
    const jobs = [...new Set([...text.matchAll(JOB_IN_WAKE)].map((m) => m[1].toUpperCase()))];
    return { kind: 'inject', origin: 'job', label: jobs.length > 1 ? 'Job wakes' : 'Job wake', source: jobs.length === 1 ? jobs[0] : null };
  }
  const head = text.slice('[controlroom]'.length).trim().split('\n')[0];
  return { kind: 'inject', origin: 'unknown', label: head.length > 80 ? `${head.slice(0, 79)}…` : head, source: null };
}

/** The origin a run's trigger implies for the message before it (for messages without an origin). */
function triggerOrigin(t: RunTrigger): EventOrigin | null {
  if (t.kind === 'schedule') return { kind: 'schedule', schedule_id: t.schedule_id, name: t.name };
  if (t.kind === 'auto_continue') return { kind: 'auto_continue' };
  if (t.kind === 'inject' && t.origin !== 'user') return { kind: 'inject', origin: t.origin, label: t.label, source: null };
  return null;
}

/** The title, model and thinking level a session-info item reports, and a new sandbox level. */
export type SessionInfo = Partial<Record<'title' | 'model' | 'thinking', string>> & { sandbox?: SandboxLevel };
const INFO_FIELDS = ['title', 'model', 'thinking'] as const;

/** A request item's request; a subagent's (shown in its parent) is answered on the subagent. */
export const requestOf = (item: ItemOf<'request'>): Request =>
  item.actor === 'root' || item.request.origin_child ? item.request : { ...item.request, origin_child: item.actor };

/** The nested-tool parent of an item, if it has one. */
const parentOf = (item: TimelineItem): string | null =>
  item.kind === 'tool_call' && item.parent_tool_call_id && item.parent_tool_call_id !== item.tool_call_id ? item.parent_tool_call_id : null;

export class Timeline {
  /** Top-level items in display order (nested tool calls are under their parent; hidden items left out). */
  rows = $state.raw<readonly ItemCell[]>([]);
  /** Nested tool calls by their parent's `tool_call_id`, in order. */
  children = $state.raw<ReadonlyMap<string, readonly ItemCell[]>>(new Map());
  /** How many old items were dropped to bound memory. */
  trimmed = $state(0);
  /** Retries folded into the run they belong to, by the run's id. */
  retries = $state.raw<ReadonlyMap<string, readonly ItemCell[]>>(new Map());
  /** How often a shown notice was repeated verbatim (2 or more), by its id. */
  repeats = $state.raw<ReadonlyMap<string, number>>(new Map());
  /** The latest state of every request in the timeline, by request id (a subagent's with its `origin_child`). */
  requests = $state.raw<ReadonlyMap<string, Request>>(new Map());
  /** The values a shown session-info item changed, by its id. */
  infoChanges = $state.raw<ReadonlyMap<string, SessionInfo>>(new Map());
  /** Messages shown as events, with their origin, by id. */
  events = $state.raw<ReadonlyMap<string, EventOrigin>>(new Map());
  /** Runs an event row already announces, by id (shown only when they don't end normally). */
  announced = $state.raw<ReadonlySet<string>>(new Set());
  /** Job wakes folded into their job's row (the job item shows the outcome), by job id. */
  wakes = $state.raw<ReadonlyMap<string, readonly ItemCell[]>>(new Map());
  /** The todo list before each todo item (empty for the first), by the item's id. */
  todosBefore = $state.raw<ReadonlyMap<string, readonly Todo[]>>(new Map());
  /** Raw transcript frames (only while a raw view asks for them). */
  readonly raw = new RawFrames();

  private readonly cells = new Map<string, ItemCell>();
  /** Tool call cells by `tool_call_id`. */
  private tools = new Map<string, ItemCell>();
  /** The row that stands for an item folded or hidden from the rows (a retry's run, a repeat's first notice). */
  private hosts = new Map<string, ItemCell>();
  /** Every cell, in order. */
  private order: ItemCell[] = [];
  private arrivals = 0;

  get size(): number {
    return this.order.length;
  }

  get(id: string): ItemCell | undefined {
    return this.cells.get(id);
  }

  /** Every item in order, nested ones included. */
  all(): readonly ItemCell[] {
    return this.order;
  }

  /** The top-level row an item is shown in (itself, the outermost tool call around it, or its host). */
  rowOf(id: string): ItemCell | undefined {
    let cell = this.cells.get(id);
    for (let hops = 0; cell && hops < this.order.length; hops++) {
      const parent = parentOf(cell.item);
      const up = parent ? this.tools.get(parent) : undefined;
      if (!up) break;
      cell = up;
    }
    return cell && (this.hosts.get(cell.id) ?? cell);
  }

  /**
   * Applies one `session.events` batch. `applied` is the head seq already reflected before it:
   * text appends and ends at or before it are replays and are skipped.
   */
  apply(ops: readonly TimelineOp[], applied: number): void {
    let structural = false;
    for (const op of ops) {
      if (op.op === 'upsert') structural = this.upsert(op.item) || structural;
      else if (op.op === 'append_text' && op.seq_to > applied) this.update(op.id, (it) => appendText(it, op.text, op.seq_to));
      else if (op.op === 'end' && op.seq_to > applied) this.update(op.id, (it) => ({ ...it, streaming: false, seq_to: Math.max(it.seq_to, op.seq_to) }));
    }
    if (structural) this.restructure();
  }

  /** Forgets everything (the transcript restarted). */
  reset(): void {
    this.cells.clear();
    this.order = [];
    this.trimmed = 0;
    this.raw.clear();
    this.restructure();
  }

  /** Inserts or replaces an item; returns whether order or nesting changed. */
  private upsert(item: TimelineItem): boolean {
    const cell = this.cells.get(item.id);
    if (!cell) {
      const fresh = new ItemCell(item, ++this.arrivals);
      this.cells.set(item.id, fresh);
      this.insert(fresh);
      return true;
    }
    const old = cell.item;
    if (item.seq_to < old.seq_to) return false;
    if (item.seq_from !== old.seq_from) {
      this.order.splice(this.indexOf(cell), 1);
      cell.item = item;
      cell.arrival = ++this.arrivals;
      this.insert(cell);
      return true;
    }
    cell.item = item;
    // Notices and session info are folded by their content; requests are collected by state.
    if (item.kind === 'notice' || item.kind === 'session_info' || item.kind === 'request' || item.kind === 'todo' || old.kind !== item.kind) return true;
    // A message's origin decides whether it is an event and which row it folds into.
    if (item.kind === 'message' && old.kind === 'message' && JSON.stringify(item.origin) !== JSON.stringify(old.origin)) return true;
    return parentOf(old) !== parentOf(item);
  }

  private update(id: string, change: (item: TimelineItem) => TimelineItem): void {
    const cell = this.cells.get(id);
    if (cell) cell.item = change(cell.item);
  }

  /** Binary insertion by (seq_from, arrival); appends are O(1). */
  private insert(cell: ItemCell): void {
    const { seq_from } = cell.item;
    let lo = 0;
    let hi = this.order.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (before(this.order[mid], seq_from, cell.arrival)) lo = mid + 1;
      else hi = mid;
    }
    this.order.splice(lo, 0, cell);
  }

  private indexOf(cell: ItemCell): number {
    let lo = 0;
    let hi = this.order.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (before(this.order[mid], cell.item.seq_from, cell.arrival)) lo = mid + 1;
      else hi = mid;
    }
    return this.order[lo] === cell ? lo : this.order.indexOf(cell);
  }

  /** Trims to MAX_ITEMS and rebuilds the rows and the nesting (O(n), on structural changes only). */
  private restructure(): void {
    const excess = this.order.length - MAX_ITEMS;
    if (excess > 0) {
      for (const cell of this.order.splice(0, excess)) this.cells.delete(cell.id);
      this.trimmed += excess;
    }
    const tools = new Map<string, ItemCell>();
    const requests = new Map<string, Request>();
    for (const cell of this.order) {
      const { item } = cell;
      if (item.kind === 'tool_call') tools.set(item.tool_call_id, cell);
      else if (item.kind === 'request') requests.set(item.request.id, requestOf(item));
    }
    const rows: ItemCell[] = [];
    const children = new Map<string, ItemCell[]>();
    const retries = new Map<string, ItemCell[]>();
    const repeats = new Map<string, number>();
    const infoChanges = new Map<string, SessionInfo>();
    const hosts = new Map<string, ItemCell>();
    const events = new Map<string, EventOrigin>();
    /** Messages whose origin was read from their text (a run's trigger names it better). */
    const guessed = new Set<string>();
    const announced = new Set<string>();
    const todosBefore = new Map<string, readonly Todo[]>();
    // Per actor: its current run, the notices seen, the session info known so far, its last user
    // message not yet followed by a run, and its last todo list.
    const runs = new Map<string, ItemCell>();
    const pending = new Map<string, ItemCell>();
    const todos = new Map<string, readonly Todo[]>();
    const notices = new Map<string, ItemCell>();
    const known = new Map<string, SessionInfo>();
    for (const cell of this.order) {
      const { item } = cell;
      const parent = parentOf(item);
      if (parent && tools.has(parent)) {
        const list = children.get(parent);
        if (list) list.push(cell);
        else children.set(parent, [cell]);
        continue;
      }
      if (hiddenInTimeline(item)) continue;
      if (item.kind === 'run') {
        runs.set(item.actor, cell);
        const before = pending.get(item.actor);
        pending.delete(item.actor);
        if (before) {
          const known = events.get(before.id);
          const implied = !known || guessed.has(before.id) ? triggerOrigin(item.trigger) : null;
          // The trigger's label, the text's job.
          if (implied) events.set(before.id, implied.kind === 'inject' && known?.kind === 'inject' ? { ...implied, source: known.source } : implied);
          if (events.has(before.id)) announced.add(cell.id);
        }
      } else if (item.kind === 'message') {
        if (item.role === 'user') {
          const fromText = item.origin ? null : textOrigin(item);
          const origin = eventOrigin(item.origin) ?? fromText;
          if (origin) events.set(cell.id, origin);
          if (fromText) guessed.add(cell.id);
          // Only a message without an origin takes one from its run's trigger.
          pending.set(item.actor, cell);
        } else pending.delete(item.actor);
      } else if (item.kind === 'todo') {
        todosBefore.set(cell.id, todos.get(item.actor) ?? []);
        todos.set(item.actor, item.items);
      } else if (item.kind === 'retry') {
        const run = runs.get(item.actor);
        if (run) {
          const list = retries.get(run.id);
          if (list) list.push(cell);
          else retries.set(run.id, [cell]);
          hosts.set(cell.id, run);
          continue;
        }
      } else if (item.kind === 'notice') {
        const key = `${item.actor}\n${item.level}\n${item.text}`;
        const first = notices.get(key);
        if (first) {
          repeats.set(first.id, (repeats.get(first.id) ?? 1) + 1);
          hosts.set(cell.id, first);
          continue;
        }
        notices.set(key, cell);
      } else if (item.kind === 'session_info') {
        const before = known.get(item.actor) ?? {};
        const now = { ...before };
        const changed: SessionInfo = {};
        for (const f of INFO_FIELDS) {
          const v = item[f];
          if (v === null) continue;
          if (before[f] !== undefined && before[f] !== v) changed[f] = v;
          now[f] = v;
        }
        if (item.sandbox) changed.sandbox = item.sandbox;
        known.set(item.actor, now);
        if (!Object.keys(changed).length) {
          const host = rows.at(-1);
          if (host) hosts.set(cell.id, host);
          continue;
        }
        infoChanges.set(cell.id, changed);
      }
      rows.push(cell);
    }
    // A job wake whose job has a row of its own folds into it.
    const jobs = new Map<string, ItemCell>();
    for (const cell of rows) if (cell.item.kind === 'job') jobs.set(cell.item.job.id, cell);
    const wakes = new Map<string, ItemCell[]>();
    const shown = rows.filter((cell) => {
      const ev = events.get(cell.id);
      const job = ev?.kind === 'inject' && ev.origin === 'job' && ev.source ? jobs.get(ev.source) : undefined;
      if (!job || !ev || ev.kind !== 'inject' || !ev.source) return true;
      const list = wakes.get(ev.source);
      if (list) list.push(cell);
      else wakes.set(ev.source, [cell]);
      hosts.set(cell.id, job);
      return false;
    });
    this.wakes = wakes;
    this.tools = tools;
    this.hosts = hosts;
    this.rows = shown;
    this.children = children;
    this.requests = requests;
    this.retries = retries;
    this.repeats = repeats;
    this.infoChanges = infoChanges;
    this.events = events;
    this.announced = announced;
    this.todosBefore = todosBefore;
  }
}

/** Appends streamed text where `append_text` says it goes (as the daemon's materializer does). */
export function appendText(item: TimelineItem, text: string, seqTo: number): TimelineItem {
  const seq_to = Math.max(item.seq_to, seqTo);
  switch (item.kind) {
    case 'message': {
      const content = item.content.slice();
      const last = content[content.length - 1];
      if (last?.kind === 'text') content[content.length - 1] = { kind: 'text', text: last.text + text };
      else content.push({ kind: 'text', text });
      return { ...item, content, seq_to };
    }
    case 'thinking':
      return { ...item, text: item.text + text, seq_to };
    case 'tool_call': {
      const out = item.output;
      const output: ToolOutput = out?.kind === 'text' || out?.kind === 'terminal' ? { kind: out.kind, text: out.text + text } : { kind: 'text', text };
      return { ...item, output, seq_to };
    }
    default:
      return { ...item, seq_to };
  }
}

/** Raw transcript frames in seq order, bounded. */
export class RawFrames {
  frames = $state.raw<readonly RawFrame[]>([]);
  /** Frames past the cap were not kept (anchored mode) or the oldest were dropped (tail mode). */
  clipped = $state(false);
  /** Anchored: keep the first frames (a seq link); otherwise keep the latest (following). */
  anchored = false;

  add(frames: readonly RawFrame[]): void {
    if (this.anchored && this.clipped) return;
    const last = this.frames.at(-1)?.seq ?? -Infinity;
    const fresh = frames.filter((f) => f.seq > last);
    if (!fresh.length) return;
    let next = [...this.frames, ...fresh];
    if (next.length > MAX_RAW) {
      next = this.anchored ? next.slice(0, MAX_RAW) : next.slice(next.length - MAX_RAW);
      this.clipped = true;
    }
    this.frames = next;
  }

  clear(): void {
    this.frames = [];
    this.clipped = false;
  }
}
