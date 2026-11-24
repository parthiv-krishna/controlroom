import { describe, expect, it } from 'vitest';
import type { FeedSession, OpenNotice, Request } from '../protocol';
import type { ItemOf } from '../state/timeline.svelte';
import { openNotices, openRequests, pinnedCount, pinnedItems } from './requests';

const req = (id: string, created_at: number, state: Request['state'] = 'pending'): Request => ({
  id, created_at, state, kind: 'permission', title: id, detail: null, body: null, blocking: true, options: [], questions: [], answer: null, origin_child: null,
});
const notice = (item_id: string, created_at: number): OpenNotice => ({ item_id, created_at, level: 'warning', text: item_id, origin_child: null });
const item = (id: string, ts: number, dismissed = false, actor = 'root'): ItemOf<'notice'> => ({
  id, ts, actor, seq_from: 1, seq_to: 1, streaming: false, kind: 'notice', level: 'error', text: id, pinned: true,
  dismissed: dismissed ? { by: null, at: ts + 1 } : null,
});
const session = (requests: Request[], notices: OpenNotice[] = []) => ({ requests, notices }) as unknown as FeedSession;
const ids = (rs: Request[]) => rs.map((r) => r.id);

describe('openRequests', () => {
  it('lists the pending requests of the summary and the timeline once, oldest first', () => {
    const timeline = new Map([['b', req('b', 2)], ['c', req('c', 1)], ['old', req('old', 0, 'answered')]]);
    expect(ids(openRequests(session([req('a', 3), req('b', 2)]), timeline))).toEqual(['c', 'b', 'a']);
  });

  it('leaves out a request the timeline already shows settled', () => {
    expect(ids(openRequests(session([req('a', 1)]), new Map([['a', req('a', 1, 'answered')]])))).toEqual([]);
  });
});

describe('openNotices', () => {
  it("lists the summary's and the timeline's open notices once, oldest first; a subagent's names it", () => {
    const timeline = new Map([['b', item('b', 2)], ['c', item('c', 1, false, 'child')], ['gone', item('gone', 0, true)]]);
    const open = openNotices(session([], [notice('a', 3), notice('b', 2)]), timeline);
    expect(open.map((n) => n.item_id)).toEqual(['c', 'b', 'a']);
    expect(open[0].origin_child).toBe('child');
  });

  it('leaves out a notice the timeline already shows dismissed', () => {
    expect(openNotices(session([], [notice('a', 1)]), new Map([['a', item('a', 1, true)]]))).toEqual([]);
  });
});

describe('the tray', () => {
  it('stacks requests and notices oldest first, and counts each', () => {
    expect(pinnedItems([req('r', 2)], [notice('n1', 1), notice('n2', 3)]).map((p) => p.id)).toEqual(['n1', 'r', 'n2']);
    expect(pinnedCount(2, 0)).toBe('2 requests');
    expect(pinnedCount(0, 3)).toBe('3 notices');
    expect(pinnedCount(1, 2)).toBe('1 request, 2 notices');
  });
});
