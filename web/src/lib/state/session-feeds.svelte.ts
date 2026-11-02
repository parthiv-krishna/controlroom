// `session.subscribe` per shown session: ref-counted, one subscription per session however many
// components show it (the open session, a subagent's live tail), resumed from the last `head_seq`
// after a reconnect without duplicates. Each feed holds the session detail and its timeline.
//
// A released feed is unsubscribed but parked with its timeline, so reopening a recent session
// resumes from its head instead of reloading; the least recently released are evicted beyond
// MAX_PARKED. Raw frames are opt-in: `showRaw` resubscribes with `raw_since`, `hideRaw` without.
import type { MachineId, SessionDetail, SessionEventsParams, OnMachine, SessionId } from '../protocol';
import { SvelteMap } from 'svelte/reactivity';
import { sessionKey } from '../model/projects';
import { Timeline } from './timeline.svelte';

/** Released feeds kept for a quick reopen. */
export const MAX_PARKED = 4;
/** Records before a seq link's target that the raw view also loads. */
const RAW_CONTEXT = 20;
/** Records the raw view loads when opened without a target (it then follows live). */
const RAW_TAIL = 300;

/** The calls the feeds need, so they can be tested without a transport. */
export interface FeedRpc {
  subscribe(
    machineId: MachineId,
    id: SessionId,
    sinceSeq: number,
    rawSince: number | null,
  ): Promise<{ subscription_id: string; head_seq: number; detail: SessionDetail }>;
  unsubscribe(subscriptionId: string): Promise<unknown>;
}

export class SessionFeed {
  detail = $state.raw<SessionDetail | null>(null);
  readonly timeline = new Timeline();
  /** Everything up to here is reflected; the resume point. */
  headSeq = 0;
  /** Raw frames after this seq are streamed; null when no raw view is open. */
  rawSince: number | null = null;
  subscriptionId: string | null = null;
  refs = 0;
  /** The subscribe call in flight, so concurrent acquires never subscribe twice. */
  inflight: Promise<void> | null = null;

  constructor(
    readonly machineId: MachineId,
    readonly id: SessionId,
  ) {}
}

/** Whether the raw frames a feed streams already include `from` (or the live tail, for null). */
function covers(feed: SessionFeed, from: number | null): boolean {
  if (feed.rawSince === null) return false;
  const { frames, anchored, clipped } = feed.timeline.raw;
  if (from === null) return !anchored;
  if (from <= feed.rawSince) return false;
  if (!clipped) return true;
  return from >= (frames[0]?.seq ?? Infinity) && from <= (frames.at(-1)?.seq ?? -Infinity);
}

export class SessionFeeds {
  /** Held feeds; reactive, so a pane can look up the feed another component holds. */
  private readonly feeds = new SvelteMap<string, SessionFeed>();
  /** Released feeds, oldest first. */
  private readonly parked = new Map<string, SessionFeed>();

  constructor(private readonly rpc: FeedRpc) {}

  /** The held feed of a session, if any. */
  get(machineId: MachineId, id: SessionId): SessionFeed | undefined {
    return this.feeds.get(sessionKey(machineId, id));
  }

  /** How many released feeds are parked (for tests). */
  get parkedCount(): number {
    return this.parked.size;
  }

  /** Holds a subscription for a session; release it with `release`. */
  acquire(machineId: MachineId, id: SessionId, online: boolean): SessionFeed {
    const key = sessionKey(machineId, id);
    let feed = this.feeds.get(key);
    if (!feed) {
      feed = this.parked.get(key) ?? new SessionFeed(machineId, id);
      this.parked.delete(key);
      this.feeds.set(key, feed);
    }
    feed.refs++;
    if (online) this.ensure(feed);
    return feed;
  }

  release(feed: SessionFeed): void {
    if (--feed.refs > 0) return;
    const key = sessionKey(feed.machineId, feed.id);
    this.feeds.delete(key);
    this.end(feed);
    feed.rawSince = null;
    feed.timeline.raw.clear();
    this.parked.set(key, feed);
    for (const old of this.parked.keys()) {
      if (this.parked.size <= MAX_PARKED) break;
      this.parked.delete(old);
    }
  }

  /** Applies a `session.events` notification to the feed it belongs to (stale ids are ignored). */
  receive(params: OnMachine<SessionEventsParams>): void {
    const feed = this.feeds.get(sessionKey(params.machine_id, params.id));
    if (!feed || feed.subscriptionId !== params.subscription_id) return;
    feed.timeline.apply(params.ops, feed.headSeq);
    if (params.raw?.length && feed.rawSince !== null) feed.timeline.raw.add(params.raw);
    feed.headSeq = Math.max(feed.headSeq, params.head_seq);
    if (params.detail) feed.detail = params.detail;
  }

  /** Streams raw frames from around `from` (a seq link), or the recent tail when null. */
  showRaw(feed: SessionFeed, from: number | null): void {
    if (covers(feed, from)) return;
    const raw = feed.timeline.raw;
    feed.rawSince = from === null ? Math.max(0, feed.headSeq - RAW_TAIL) : Math.max(0, from - 1 - RAW_CONTEXT);
    raw.clear();
    raw.anchored = from !== null;
    this.resubscribe(feed);
  }

  /** Stops the raw stream. */
  hideRaw(feed: SessionFeed): void {
    if (feed.rawSince === null) return;
    feed.rawSince = null;
    feed.timeline.raw.clear();
    this.resubscribe(feed);
  }

  /** After a (re)connect: the old subscriptions died with the socket; resume every held feed. */
  resubscribeAll(): void {
    for (const feed of this.feeds.values()) {
      feed.subscriptionId = null;
      feed.inflight = null;
      this.ensure(feed);
    }
  }

  /** Forgets a removed session's feed, held or parked. */
  drop(machineId: MachineId, id: SessionId): void {
    const key = sessionKey(machineId, id);
    this.parked.delete(key);
    const feed = this.feeds.get(key);
    if (!feed) return;
    this.feeds.delete(key);
    this.end(feed);
  }

  /** Ends a feed's subscription; an in-flight subscribe is ended once it resolves. */
  private end(feed: SessionFeed): void {
    if (feed.subscriptionId) this.unsubscribe(feed.subscriptionId);
    feed.subscriptionId = null;
    feed.inflight = null;
  }

  private ensure(feed: SessionFeed): void {
    if (!feed.subscriptionId && !feed.inflight) this.subscribe(feed);
  }

  /** Replaces a live subscription (new raw settings); offline feeds pick them up on reconnect. */
  private resubscribe(feed: SessionFeed): void {
    if (feed.subscriptionId || feed.inflight) this.subscribe(feed);
  }

  private subscribe(feed: SessionFeed): void {
    const attempt = this.rpc
      .subscribe(feed.machineId, feed.id, feed.headSeq, feed.rawSince)
      .then((r) => {
        if (feed.inflight !== attempt || feed.refs <= 0) {
          // Superseded (released, reconnected or resubscribed): end the orphan.
          this.unsubscribe(r.subscription_id);
          return;
        }
        feed.inflight = null;
        const old = feed.subscriptionId;
        feed.subscriptionId = r.subscription_id;
        if (old) this.unsubscribe(old);
        // A head behind ours on a fresh subscribe means the transcript restarted.
        if (!old && r.head_seq < feed.headSeq) {
          feed.timeline.reset();
          feed.headSeq = 0;
        }
        feed.headSeq = Math.max(feed.headSeq, r.head_seq);
        feed.detail = r.detail;
      })
      .catch(() => {
        if (feed.inflight === attempt) feed.inflight = null;
      });
    feed.inflight = attempt;
  }

  private unsubscribe(subscriptionId: string): void {
    void this.rpc.unsubscribe(subscriptionId).catch(() => {});
  }
}
