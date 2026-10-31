// `session.subscribe` per open session: ref-counted, one subscription per session however many
// components show it, resumed from the last `head_seq` after a reconnect.
//
// P2c keeps only the session detail (queue, todos, auto-continue, jobs, peers). The timeline ops
// in `session.events` are the P4 session-stream store's to consume: it plugs in here, next to
// `detail`, and renders into the session pane's timeline slot.
import type { MachineId, SessionDetail, SessionEventsParams, OnMachine, SessionId } from '../protocol';
import { sessionKey } from '../model/projects';

/** The calls the feeds need, so they can be tested without a transport. */
export interface FeedRpc {
  subscribe(machineId: MachineId, id: SessionId, sinceSeq: number): Promise<{ subscription_id: string; head_seq: number; detail: SessionDetail }>;
  unsubscribe(subscriptionId: string): Promise<unknown>;
}

export class SessionFeed {
  detail = $state.raw<SessionDetail | null>(null);
  /** Resume point after a reconnect. */
  headSeq = 0;
  subscriptionId: string | null = null;
  refs = 0;
  /** The subscribe call in flight, so concurrent acquires never subscribe twice. */
  inflight: Promise<void> | null = null;

  constructor(
    readonly machineId: MachineId,
    readonly id: SessionId,
  ) {}
}

export class SessionFeeds {
  private readonly feeds = new Map<string, SessionFeed>();

  constructor(private readonly rpc: FeedRpc) {}

  /** Holds a subscription for a session; release it with `release`. */
  acquire(machineId: MachineId, id: SessionId, online: boolean): SessionFeed {
    const key = sessionKey(machineId, id);
    let feed = this.feeds.get(key);
    if (!feed) this.feeds.set(key, (feed = new SessionFeed(machineId, id)));
    feed.refs++;
    if (online) this.ensure(feed);
    return feed;
  }

  release(feed: SessionFeed): void {
    if (--feed.refs > 0) return;
    this.feeds.delete(sessionKey(feed.machineId, feed.id));
    // An in-flight subscribe is ended once it resolves (see ensure).
    if (feed.subscriptionId) void this.rpc.unsubscribe(feed.subscriptionId).catch(() => {});
    feed.subscriptionId = null;
  }

  /** Applies a `session.events` notification to the feed it belongs to (stale ids are ignored). */
  receive(params: OnMachine<SessionEventsParams>): void {
    const feed = this.feeds.get(sessionKey(params.machine_id, params.id));
    if (!feed || feed.subscriptionId !== params.subscription_id) return;
    feed.headSeq = Math.max(feed.headSeq, params.head_seq);
    if (params.detail) feed.detail = params.detail;
  }

  /** After a (re)connect: the old subscriptions died with the socket; resume every held feed. */
  resubscribeAll(): void {
    for (const feed of this.feeds.values()) {
      feed.subscriptionId = null;
      feed.inflight = null;
      this.ensure(feed);
    }
  }

  /** Forgets the feeds of a removed session. */
  drop(machineId: MachineId, id: SessionId): void {
    const feed = this.feeds.get(sessionKey(machineId, id));
    if (!feed) return;
    feed.refs = 1;
    this.release(feed);
  }

  private ensure(feed: SessionFeed): void {
    if (feed.subscriptionId || feed.inflight) return;
    const attempt = this.rpc
      .subscribe(feed.machineId, feed.id, feed.headSeq)
      .then((r) => {
        if (feed.inflight !== attempt) return; // superseded by a reconnect
        feed.inflight = null;
        if (feed.refs <= 0) {
          void this.rpc.unsubscribe(r.subscription_id).catch(() => {});
          return;
        }
        feed.subscriptionId = r.subscription_id;
        feed.headSeq = Math.max(feed.headSeq, r.head_seq);
        feed.detail = r.detail;
      })
      .catch(() => {
        if (feed.inflight === attempt) feed.inflight = null;
      });
    feed.inflight = attempt;
  }
}
