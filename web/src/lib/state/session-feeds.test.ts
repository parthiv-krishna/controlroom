import { describe, expect, it, vi } from 'vitest';
import type { SessionDetail } from '../protocol';
import { SessionFeeds, type FeedRpc } from './session-feeds.svelte';

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
    expect(r.subscribe).toHaveBeenLastCalledWith('m', 's', 25);
  });

  it('waits for the connection before subscribing', () => {
    const r = rpc();
    const feeds = new SessionFeeds(r);
    feeds.acquire('m', 's', false);
    expect(r.subscribe).not.toHaveBeenCalled();
    feeds.resubscribeAll();
    expect(r.subscribe).toHaveBeenCalledWith('m', 's', 0);
  });
});
