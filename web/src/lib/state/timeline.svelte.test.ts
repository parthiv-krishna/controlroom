// Fine-grained reactivity of the timeline store: a streamed token re-runs only what reads that item.
import { flushSync } from 'svelte';
import { describe, expect, it } from 'vitest';
import type { TimelineItem } from '../protocol';
import { Timeline } from './timeline.svelte';

const msg = (id: string, seq: number): TimelineItem => ({
  id, seq_from: seq, seq_to: seq, actor: 'root', ts: 0, streaming: true, kind: 'message', role: 'agent', content: [],
});

describe('Timeline reactivity', () => {
  it('notifies only the streamed item, and the rows only on structural changes', () => {
    const t = new Timeline();
    t.apply([{ op: 'upsert', item: msg('a', 1) }, { op: 'upsert', item: msg('b', 2) }], 0);
    const runs = { rows: 0, a: 0, b: 0 };
    const stop = $effect.root(() => {
      $effect(() => {
        void t.rows.map((c) => c.id);
        runs.rows++;
      });
      $effect(() => {
        void t.get('a')!.item;
        runs.a++;
      });
      $effect(() => {
        void t.get('b')!.item;
        runs.b++;
      });
    });
    flushSync();
    expect(runs).toEqual({ rows: 1, a: 1, b: 1 });
    for (let seq = 3; seq < 13; seq++) {
      t.apply([{ op: 'append_text', id: 'a', text: 'x', seq_from: seq, seq_to: seq }], seq - 1);
      flushSync();
    }
    expect(runs).toEqual({ rows: 1, a: 11, b: 1 });
    t.apply([{ op: 'upsert', item: msg('c', 20) }], 12);
    flushSync();
    expect(runs).toEqual({ rows: 2, a: 11, b: 1 });
    stop();
  });
});
