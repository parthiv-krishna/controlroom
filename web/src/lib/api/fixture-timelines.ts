// Recorded session timelines for the fixture transport (web/fixtures/timelines.json, generated
// from the PiDriver goldens by scripts/timeline-fixtures.mjs). A replay serves what the daemon
// would: a backlog of the items changed after `since_seq` (plus the raw frames after `raw_since`),
// then live steps in order. It interprets nothing beyond those seq filters.
import type { RawFrame, TimelineItem, TimelineOp } from '../protocol';

/** One live `session.events` batch: the ops (and raw frames) of one transcript record. */
export interface FixtureLiveStep {
  delay_ms: number;
  head_seq: number;
  ops: TimelineOp[];
  raw: RawFrame[];
}

export interface FixtureTimeline {
  /** The head the backlog reflects. */
  head_seq: number;
  /** The materialized timeline at `head_seq`. */
  items: TimelineItem[];
  raw: RawFrame[];
  /** Played after the session is subscribed. */
  live: FixtureLiveStep[];
}

/** One session's recorded timeline and how far its live steps have played. */
export class TimelineReplay {
  private played = 0;

  constructor(private readonly timeline: FixtureTimeline) {}

  /** The head after the steps played so far. */
  get head(): number {
    return this.played ? this.timeline.live[this.played - 1].head_seq : this.timeline.head_seq;
  }

  /** What a subscribe sends first: upserts of items changed after `since`, then the played steps after it. */
  backlog(since: number, rawSince: number | null): { ops: TimelineOp[]; raw: RawFrame[] } {
    const steps = this.timeline.live.slice(0, this.played).filter((s) => s.head_seq > since);
    const ops: TimelineOp[] = [
      ...this.timeline.items.filter((it) => it.seq_to > since).map((item): TimelineOp => ({ op: 'upsert', item })),
      ...steps.flatMap((s) => s.ops),
    ];
    const raw = rawSince === null ? [] : [...this.timeline.raw, ...this.timeline.live.slice(0, this.played).flatMap((s) => s.raw)].filter((f) => f.seq > rawSince);
    return { ops, raw };
  }

  /** The next live step, if any, without playing it. */
  peek(): FixtureLiveStep | undefined {
    return this.timeline.live[this.played];
  }

  /** Plays the next live step. */
  take(): FixtureLiveStep | undefined {
    const step = this.timeline.live[this.played];
    if (step) this.played++;
    return step;
  }
}
