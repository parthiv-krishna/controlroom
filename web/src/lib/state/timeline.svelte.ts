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
import type { RawFrame, TimelineItem, TimelineOp, ToolOutput } from '../protocol';

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
 * Items kept but not shown in the reading view: a turn's usage (the header's statusline has the
 * session's totals; the raw view still has each turn's).
 */
export const hiddenInTimeline = (item: TimelineItem): boolean => item.kind === 'usage' && item.scope === 'turn';

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
  /** Ids of the requests that have a timeline item (others are shown from the summary). */
  requestIds = $state.raw<ReadonlySet<string>>(new Set());
  /** Raw transcript frames (only while a raw view asks for them). */
  readonly raw = new RawFrames();

  private readonly cells = new Map<string, ItemCell>();
  /** Tool call cells by `tool_call_id`. */
  private tools = new Map<string, ItemCell>();
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

  /** The top-level row an item is shown in (itself, or the outermost tool call around it). */
  rowOf(id: string): ItemCell | undefined {
    let cell = this.cells.get(id);
    for (let hops = 0; cell && hops < this.order.length; hops++) {
      const parent = parentOf(cell.item);
      const up = parent ? this.tools.get(parent) : undefined;
      if (!up) return cell;
      cell = up;
    }
    return cell;
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
    return parentOf(old) !== parentOf(item) || (old.kind === 'tool_call') !== (item.kind === 'tool_call');
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
    const requests = new Set<string>();
    for (const cell of this.order) {
      const { item } = cell;
      if (item.kind === 'tool_call') tools.set(item.tool_call_id, cell);
      else if (item.kind === 'request') requests.add(item.request.id);
    }
    const rows: ItemCell[] = [];
    const children = new Map<string, ItemCell[]>();
    for (const cell of this.order) {
      const parent = parentOf(cell.item);
      if (parent && tools.has(parent)) {
        const list = children.get(parent);
        if (list) list.push(cell);
        else children.set(parent, [cell]);
      } else if (!hiddenInTimeline(cell.item)) rows.push(cell);
    }
    this.tools = tools;
    this.rows = rows;
    this.children = children;
    if (requests.size !== this.requestIds.size || [...requests].some((id) => !this.requestIds.has(id))) this.requestIds = requests;
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
