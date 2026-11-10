// A session's open requests, as pinned above its composer.
import type { FeedSession, Request } from '../protocol';

/**
 * The session's open requests, oldest first: the summary's pending ones (its subagents' included)
 * and the timeline's, which may arrive first. A request the timeline already shows settled is
 * left out (the summary catches up).
 */
export function openRequests(s: FeedSession, timeline: ReadonlyMap<string, Request>): Request[] {
  const open = new Map<string, Request>();
  for (const r of s.requests) if (r.state === 'pending' && (timeline.get(r.id)?.state ?? 'pending') === 'pending') open.set(r.id, r);
  for (const r of timeline.values()) if (r.state === 'pending' && !open.has(r.id)) open.set(r.id, r);
  return [...open.values()].sort((a, b) => a.created_at - b.created_at);
}

/** Scrolls a pinned request into view, flashes it and focuses its first control. */
export function revealPinned(requestId: string): void {
  const el = document.querySelector<HTMLElement>(`[data-pinned="${CSS.escape(requestId)}"]`);
  if (!el) return;
  el.scrollIntoView({ block: 'nearest' });
  el.querySelector<HTMLElement>('button:not([disabled]), input, textarea')?.focus({ preventScroll: true });
  el.classList.add('flash');
  setTimeout(() => el.classList.remove('flash'), 1500);
}
