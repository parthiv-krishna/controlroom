// What every timeline item component needs from the timeline it is shown in.
import { getContext, setContext } from 'svelte';
import type { MachineSession, TimelineItem } from '../protocol';
import type { Timeline } from '../state/timeline.svelte';

export interface TimelineContext {
  /** The session the timeline belongs to (its current summary). */
  readonly s: MachineSession;
  readonly timeline: Timeline;
  /** Whether an item is expanded (per session and item, so ids never collide across sessions). */
  isOpen(id: string, fallback: boolean): boolean;
  toggle(id: string, fallback: boolean): void;
  /** Opens the raw view at an item's records. */
  showRaw(item: TimelineItem): void;
}

const KEY = Symbol('timeline');
export const provideTimeline = (ctx: TimelineContext): TimelineContext => setContext(KEY, ctx);
export const useTimeline = (): TimelineContext => getContext<TimelineContext>(KEY);
