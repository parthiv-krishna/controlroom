import { describe, expect, it } from 'vitest';
import type { TimelineItem, TimelineOp, ToolOutput } from '../protocol';
import { MAX_ITEMS, MAX_RAW, RawFrames, Timeline } from './timeline.svelte';

const base = { actor: 'root', ts: 0, streaming: false } as const;
const msg = (id: string, seq_from: number, seq_to = seq_from, text = '', streaming = false): TimelineItem => ({
  ...base, id, seq_from, seq_to, streaming, kind: 'message', role: 'agent', content: text ? [{ kind: 'text', text }] : [],
});
const tool = (id: string, seq: number, parent: string | null = null, output: ToolOutput | null = null): TimelineItem => ({
  ...base, id, seq_from: seq, seq_to: seq, kind: 'tool_call', tool_call_id: id, parent_tool_call_id: parent, name: 'bash', title: id,
  tool_kind: 'execute', input: null, status: 'running', output, exit: null, duration_ms: null,
});
const up = (item: TimelineItem): TimelineOp => ({ op: 'upsert', item });
const append = (id: string, text: string, seq: number): TimelineOp => ({ op: 'append_text', id, text, seq_from: seq, seq_to: seq });
const ids = (t: Timeline) => t.rows.map((c) => c.item.id);
const text = (t: Timeline, id: string) => {
  const it = t.get(id)?.item;
  if (it?.kind === 'message') return it.content.map((p) => (p.kind === 'text' ? p.text : '')).join('');
  if (it?.kind === 'thinking') return it.text;
  if (it?.kind === 'tool_call') return it.output && 'text' in it.output ? it.output.text : null;
  return null;
};

describe('Timeline', () => {
  it('orders by seq_from, then arrival', () => {
    const t = new Timeline();
    t.apply([up(msg('b', 5)), up(msg('a', 2)), up(msg('c', 5)), up(msg('d', 9))], 0);
    expect(ids(t)).toEqual(['a', 'b', 'c', 'd']);
  });

  it('moves an item re-upserted with a later seq_from (a delivered steer)', () => {
    const t = new Timeline();
    t.apply([up(msg('user-3', 3)), up(msg('run-4', 4)), up(msg('msg-6', 6))], 0);
    t.apply([up(msg('user-3', 8, 8))], 6);
    expect(ids(t)).toEqual(['run-4', 'msg-6', 'user-3']);
    expect(t.size).toBe(3);
  });

  it('appends text in place: one token replaces exactly one item', () => {
    const t = new Timeline();
    t.apply([up(msg('m', 1, 1, '', true)), up(msg('n', 2))], 0);
    const rows = t.rows;
    const other = t.get('n')!.item;
    t.apply([append('m', 'Hel', 3)], 2);
    t.apply([append('m', 'lo', 4)], 3);
    expect(text(t, 'm')).toBe('Hello');
    expect(t.rows).toBe(rows); // no structural change
    expect(t.get('n')!.item).toBe(other);
    t.apply([{ op: 'end', id: 'm', seq_from: 5, seq_to: 5 }], 4);
    expect(t.get('m')!.item).toMatchObject({ streaming: false, seq_to: 5 });
  });

  it('appends to thinking text and to text or terminal tool output', () => {
    const t = new Timeline();
    t.apply([up({ ...base, id: 'th', seq_from: 1, seq_to: 1, kind: 'thinking', text: 'a' }), up(tool('x', 2, null, { kind: 'terminal', text: '$ ' })), up(tool('y', 3))], 0);
    t.apply([append('th', 'b', 4), append('x', 'ok', 4), append('y', 'out', 4)], 3);
    expect(text(t, 'th')).toBe('ab');
    expect(t.get('x')!.item).toMatchObject({ output: { kind: 'terminal', text: '$ ok' } });
    expect(t.get('y')!.item).toMatchObject({ output: { kind: 'text', text: 'out' } });
  });

  it('skips replayed appends and stale upserts on resume (no duplicates)', () => {
    const t = new Timeline();
    t.apply([up(msg('m', 1, 1, '', true))], 0);
    t.apply([append('m', 'ab', 2)], 1);
    t.apply([append('m', 'cd', 3)], 2);
    // The same ops again (an overlapping subscription), then a backlog upsert from before them.
    t.apply([append('m', 'ab', 2), append('m', 'cd', 3)], 3);
    t.apply([up(msg('m', 1, 2, 'ab', true))], 3);
    expect(text(t, 'm')).toBe('abcd');
    // A backlog upsert at the same point replaces the item whole.
    t.apply([up(msg('m', 1, 3, 'abcd', false))], 3);
    expect(t.get('m')!.item.streaming).toBe(false);
  });

  it('nests tool calls under their parent, whichever arrives first', () => {
    const t = new Timeline();
    t.apply([up(tool('child', 4, 'parent')), up(msg('m', 1))], 0);
    expect(ids(t)).toEqual(['m', 'child']); // parent unknown: shown at the top level
    t.apply([up(tool('parent', 3))], 4);
    expect(ids(t)).toEqual(['m', 'parent']);
    expect(t.children.get('parent')!.map((c) => c.item.id)).toEqual(['child']);
  });

  it("keeps a turn's usage but leaves it out of the rows; a session's usage stays", () => {
    const usage = (id: string, seq: number, scope: 'turn' | 'session'): TimelineItem => ({
      ...base, id, seq_from: seq, seq_to: seq, kind: 'usage', scope, tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 }, cost: null, context: null,
    });
    const t = new Timeline();
    t.apply([up(msg('a', 1)), up(usage('u1', 2, 'turn')), up(msg('b', 3)), up(usage('u2', 4, 'session'))], 0);
    expect(ids(t)).toEqual(['a', 'b', 'u2']);
    expect(t.get('u1')?.item.kind).toBe('usage');
    expect(t.size).toBe(4);
  });

  it('ignores ops for unknown items', () => {
    const t = new Timeline();
    t.apply([append('nope', 'x', 1), { op: 'end', id: 'nope', seq_from: 1, seq_to: 1 }, { op: 'unknown' }], 0);
    expect(t.size).toBe(0);
  });

  it('trims the oldest items beyond MAX_ITEMS', () => {
    const t = new Timeline();
    const ops = Array.from({ length: MAX_ITEMS + 10 }, (_, i) => up(msg(`m${i}`, i + 1)));
    t.apply(ops, 0);
    expect(t.size).toBe(MAX_ITEMS);
    expect(t.trimmed).toBe(10);
    expect(t.rows[0].item.id).toBe('m10');
    expect(t.get('m0')).toBeUndefined();
  });

  it('resets', () => {
    const t = new Timeline();
    t.apply([up(msg('m', 1))], 0);
    t.reset();
    expect(t.size).toBe(0);
    expect(t.rows).toEqual([]);
  });
});

describe('Timeline requests', () => {
  it('keeps the latest state of each request, a subagent\'s with its origin', () => {
    const r = (state: 'pending' | 'answered', actor = 'root'): TimelineItem => ({
      ...base, actor, id: `i-${actor}`, seq_from: 1, seq_to: state === 'pending' ? 1 : 2, kind: 'request',
      request: { id: `r-${actor}`, created_at: 0, state, kind: 'dialog', title: '', detail: null, body: null, blocking: true, options: [], questions: [], answer: null, origin_child: null },
    });
    const t = new Timeline();
    t.apply([up(r('pending')), up(r('pending', 'child'))], 0);
    expect(t.requests.get('r-root')?.state).toBe('pending');
    expect(t.requests.get('r-child')?.origin_child).toBe('child');
    t.apply([up(r('answered'))], 1);
    expect(t.requests.get('r-root')?.state).toBe('answered');
  });
});

describe('Timeline folding', () => {
  const run = (id: string, seq: number, actor = 'root'): TimelineItem => ({
    ...base, actor, id, seq_from: seq, seq_to: seq, kind: 'run', trigger: { kind: 'user', device: null }, state: 'settled', outcome: 'completed', error: null,
  });
  const retry = (id: string, seq: number, attempt: number): TimelineItem => ({
    ...base, id, seq_from: seq, seq_to: seq, kind: 'retry', attempt, max_attempts: 3, phase: 'succeeded', delay_ms: null, error: '503',
  });
  const notice = (id: string, seq: number, text: string): TimelineItem => ({ ...base, id, seq_from: seq, seq_to: seq, kind: 'notice', level: 'info', text });
  const info = (id: string, seq: number, fields: { model?: string; thinking?: string }): TimelineItem => ({
    ...base, id, seq_from: seq, seq_to: seq, kind: 'session_info', title: null, model: fields.model ?? null, thinking: fields.thinking ?? null,
  });

  it('folds retries into their run; a retry before any run stays a row', () => {
    const t = new Timeline();
    t.apply([up(retry('r0', 1, 1)), up(run('run', 2)), up(msg('m', 3)), up(retry('r1', 4, 1)), up(retry('r2', 5, 2))], 0);
    expect(ids(t)).toEqual(['r0', 'run', 'm']);
    expect(t.retries.get('run')?.map((c) => c.id)).toEqual(['r1', 'r2']);
    expect(t.rowOf('r2')?.id).toBe('run');
  });

  it('shows a repeated notice once, with its count', () => {
    const t = new Timeline();
    t.apply([up(notice('a', 1, 'ready')), up(msg('m', 2)), up(notice('b', 3, 'ready')), up(notice('c', 4, 'other')), up(notice('d', 5, 'ready'))], 0);
    expect(ids(t)).toEqual(['a', 'm', 'c']);
    expect(t.repeats.get('a')).toBe(3);
    expect(t.repeats.has('c')).toBe(false);
    expect(t.rowOf('d')?.id).toBe('a');
  });

  it('shows only session info that changed, never the first values', () => {
    const t = new Timeline();
    t.apply([up(msg('m', 1)), up(info('i1', 2, { model: 'A', thinking: 'low' })), up(info('i2', 3, { model: 'A' })), up(info('i3', 4, { model: 'B', thinking: 'low' }))], 0);
    expect(ids(t)).toEqual(['m', 'i3']);
    expect(t.infoChanges.get('i3')).toEqual({ model: 'B' });
    expect(t.rowOf('i1')?.id).toBe('m');
  });
});

describe('RawFrames', () => {
  const frame = (seq: number) => ({ seq, ts: 0, stream: 'from_agent' as const, text: `{"n":${seq}}` });

  it('keeps frames in order without duplicates', () => {
    const r = new RawFrames();
    r.add([frame(1), frame(2)]);
    r.add([frame(2), frame(3)]);
    expect(r.frames.map((f) => f.seq)).toEqual([1, 2, 3]);
  });

  it('follows the tail, or stays anchored at a seq link', () => {
    const tail = new RawFrames();
    tail.add(Array.from({ length: MAX_RAW + 5 }, (_, i) => frame(i + 1)));
    expect(tail.frames[0].seq).toBe(6);
    const anchored = new RawFrames();
    anchored.anchored = true;
    anchored.add(Array.from({ length: MAX_RAW + 5 }, (_, i) => frame(i + 1)));
    anchored.add([frame(MAX_RAW + 9)]);
    expect(anchored.frames[0].seq).toBe(1);
    expect(anchored.frames.at(-1)!.seq).toBe(MAX_RAW);
    expect(anchored.clipped).toBe(true);
  });
});
