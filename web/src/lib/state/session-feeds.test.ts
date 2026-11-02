import { describe, expect, it, vi } from 'vitest';
import type { SessionDetail, SessionEventsParams, TimelineItem } from '../protocol';
import { MAX_PARKED, SessionFeeds, type FeedRpc } from './session-feeds.svelte';

const detail = (n: number): SessionDetail => ({
  agent_sessions: [],
  queue: [],
  todos: [],
  auto_continue: { status: 'off', reason: null, runs_used: n, budget: 0 },
  jobs: [],
  peers: [],
});

function rpc() {
  let n = 0;
  const resolvers: (() => void)[] = [];
  const subscribe = vi.fn<FeedRpc['subscribe']>(
    (_m, _id, since) =>
      new Promise((resolve) => {
        const sub = `sub-${++n}`;
        resolvers.push(() => resolve({ subscription_id: sub, head_seq: since + 10, detail: detail(n) }));
      }),
  );
  const unsubscribe = vi.fn<FeedRpc['unsubscribe']>(async () => ({}));
  return { subscribe, unsubscribe, resolveAll: () => resolvers.splice(0).forEach((r) => r()) };
}
const message = (id: string, seq: number, text: string): TimelineItem => ({
  id, seq_from: seq, seq_to: seq, actor: 'root', ts: 0, streaming: true, kind: 'message', role: 'agent', content: text ? [{ kind: 'text', text }] : [],
});
const tick = () => new Promise((r) => setTimeout(r, 0));

describe('SessionFeeds', () => {
  it('subscribes once per session however many hold it', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const a = feeds.acquire('m', 's', true);
    const b = feeds.acquire('m', 's', true);
    expect(a).toBe(b);
    r.resolveAll();
    await tick();
    expect(r.subscribe).toHaveBeenCalledTimes(1);
    expect(a.detail?.auto_continue.runs_used).toBe(1);
    feeds.release(a);
    expect(r.unsubscribe).not.toHaveBeenCalled();
    feeds.release(b);
    expect(r.unsubscribe).toHaveBeenCalledWith('sub-1');
  });

  it('ends a subscription that resolves after the last release (no leak)', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const feed = feeds.acquire('m', 's', true);
    feeds.release(feed);
    r.resolveAll();
    await tick();
    expect(r.unsubscribe).toHaveBeenCalledWith('sub-1');
  });

  it('applies events of its own subscription only and resumes from head_seq', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const feed = feeds.acquire('m', 's', true);
    r.resolveAll();
    await tick();
    feeds.receive({ machine_id: 'm', id: 's', subscription_id: 'stale', head_seq: 99, ops: [], detail: detail(7) });
    expect(feed.headSeq).toBe(10);
    feeds.receive({ machine_id: 'm', id: 's', subscription_id: 'sub-1', head_seq: 25, ops: [], detail: detail(7) });
    expect(feed.headSeq).toBe(25);
    expect(feed.detail?.auto_continue.runs_used).toBe(7);
    feeds.resubscribeAll();
    expect(r.subscribe).toHaveBeenLastCalledWith('m', 's', 25, null);
  });

  it('waits for the connection before subscribing', () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    feeds.acquire('m', 's', false);
    expect(r.subscribe).not.toHaveBeenCalled();
    feeds.resubscribeAll();
    expect(r.subscribe).toHaveBeenCalledWith('m', 's', 0, null);
  });
  it('applies timeline ops and resumes from head_seq without duplicates after a reconnect', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const feed = feeds.acquire('m', 's', true);
    r.resolveAll();
    await tick();
    const ev = (sub: string, head_seq: number, ops: SessionEventsParams['ops']) => feeds.receive({ machine_id: 'm', id: 's', subscription_id: sub, head_seq, ops });
    ev('sub-1', 11, [{ op: 'upsert', item: message('a', 11, '') }]);
    ev('sub-1', 12, [{ op: 'append_text', id: 'a', text: 'Hi', seq_from: 12, seq_to: 12 }]);
    feeds.resubscribeAll();
    expect(r.subscribe).toHaveBeenLastCalledWith('m', 's', 12, null);
    r.resolveAll();
    await tick();
    // The backlog (upserts of items changed after 12) and a replayed append are harmless.
    ev('sub-2', 22, [{ op: 'append_text', id: 'a', text: 'Hi', seq_from: 12, seq_to: 12 }, { op: 'append_text', id: 'a', text: '!', seq_from: 13, seq_to: 13 }]);
    const item = feed.timeline.get('a')!.item;
    expect(item.kind === 'message' && item.content).toEqual([{ kind: 'text', text: 'Hi' }]);
  });

  it('parks released feeds and evicts the oldest beyond the cap', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const first = feeds.acquire('m', 's0', true);
    r.resolveAll();
    await tick();
    feeds.receive({ machine_id: 'm', id: 's0', subscription_id: 'sub-1', head_seq: 30, ops: [{ op: 'upsert', item: message('x', 20, 'kept') }] });
    feeds.release(first);
    expect(feeds.get('m', 's0')).toBeUndefined();
    // Reopened: the same timeline, resumed from its head.
    const again = feeds.acquire('m', 's0', true);
    expect(again).toBe(first);
    expect(again.timeline.get('x')).toBeDefined();
    expect(r.subscribe).toHaveBeenLastCalledWith('m', 's0', 30, null);
    feeds.release(again);
    for (let i = 1; i <= MAX_PARKED; i++) feeds.release(feeds.acquire('m', `s${i}`, false));
    expect(feeds.parkedCount).toBe(MAX_PARKED);
    expect(feeds.acquire('m', 's0', false)).not.toBe(first);
  });

  it('drops a removed session entirely', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    feeds.acquire('m', 's', true);
    r.resolveAll();
    await tick();
    feeds.drop('m', 's');
    expect(r.unsubscribe).toHaveBeenCalledWith('sub-1');
    expect(feeds.get('m', 's')).toBeUndefined();
    expect(feeds.parkedCount).toBe(0);
  });

  it('opens raw frames by resubscribing with raw_since and swaps subscriptions', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const feed = feeds.acquire('m', 's', true);
    r.resolveAll();
    await tick();
    feeds.receive({ machine_id: 'm', id: 's', subscription_id: 'sub-1', head_seq: 50, ops: [] });
    feeds.showRaw(feed, 40);
    expect(r.subscribe).toHaveBeenLastCalledWith('m', 's', 50, 19);
    // Events of the old subscription still apply until the new one answers.
    feeds.receive({ machine_id: 'm', id: 's', subscription_id: 'sub-1', head_seq: 51, ops: [], raw: [{ seq: 51, ts: 0, stream: 'from_agent', text: 'x' }] });
    r.resolveAll();
    await tick();
    expect(feed.subscriptionId).toBe('sub-2');
    expect(r.unsubscribe).toHaveBeenCalledWith('sub-1');
    feeds.receive({ machine_id: 'm', id: 's', subscription_id: 'sub-2', head_seq: 51, ops: [], raw: [{ seq: 20, ts: 0, stream: 'to_agent', text: 'y' }] });
    expect(feed.timeline.raw.frames.map((f) => f.seq)).toEqual([51]);
    // Already covered: no new subscription.
    feeds.showRaw(feed, 45);
    expect(r.subscribe).toHaveBeenCalledTimes(2);
    feeds.hideRaw(feed);
    expect(r.subscribe).toHaveBeenLastCalledWith('m', 's', 60, null); // the mock's head after the raw resubscribe
    expect(feed.timeline.raw.frames).toEqual([]);
  });

  it('resets the timeline when a fresh subscribe reports an earlier head', async () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    const feed = feeds.acquire('m', 's', true);
    r.resolveAll();
    await tick();
    feeds.receive({ machine_id: 'm', id: 's', subscription_id: 'sub-1', head_seq: 90, ops: [{ op: 'upsert', item: message('old', 80, 'x') }] });
    feeds.resubscribeAll();
    r.subscribe.mockImplementationOnce(async () => ({ subscription_id: 'sub-x', head_seq: 5, detail: detail(0) }));
    feeds.resubscribeAll();
    await tick();
    expect(feed.timeline.size).toBe(0);
    expect(feed.headSeq).toBe(5);
  });
});
