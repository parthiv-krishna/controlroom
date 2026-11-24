// A session's open requests and agent notices, as pinned above its composer.
import type { FeedSession, OpenNotice, Request } from '../protocol';
import type { ItemOf } from '../state/timeline.svelte';

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

/**
 * The session's open agent notices, oldest first: the summary's (its subagents' included) and the
 * timeline's pinned ones not yet dismissed, which may arrive first. One the timeline already shows
 * dismissed is left out.
 */
export function openNotices(s: FeedSession, timeline: ReadonlyMap<string, ItemOf<'notice'>>): OpenNotice[] {
  const open = new Map<string, OpenNotice>();
  for (const n of s.notices) if (!timeline.get(n.item_id)?.dismissed) open.set(n.item_id, n);
  for (const it of timeline.values()) {
    if (it.dismissed || open.has(it.id)) continue;
    open.set(it.id, { item_id: it.id, level: it.level, text: it.text, created_at: it.ts, origin_child: it.actor === 'root' ? null : it.actor });
  }
  return [...open.values()].sort((a, b) => a.created_at - b.created_at);
}

/** What the tray pins, oldest first. */
export type Pinned = { kind: 'request'; id: string; at: number; req: Request } | { kind: 'notice'; id: string; at: number; notice: OpenNotice };

/** Open requests and notices in one list, oldest first. */
export function pinnedItems(requests: Request[], notices: OpenNotice[]): Pinned[] {
  return [
    ...requests.map((req): Pinned => ({ kind: 'request', id: req.id, at: req.created_at, req })),
    ...notices.map((notice): Pinned => ({ kind: 'notice', id: notice.item_id, at: notice.created_at, notice })),
  ].sort((a, b) => a.at - b.at);
}

/** The tray's count: "2 requests", "3 notices", "1 request, 2 notices". */
export function pinnedCount(requests: number, notices: number): string {
  const n = (count: number, one: string) => `${count} ${one}${count === 1 ? '' : 's'}`;
  return [requests && n(requests, 'request'), notices && n(notices, 'notice')].filter(Boolean).join(', ');
}

/** Scrolls a pinned request or notice into view, flashes it and focuses its first control. */
export function revealPinned(id: string): void {
  const el = document.querySelector<HTMLElement>(`[data-pinned="${CSS.escape(id)}"]`);
  if (!el) return;
  el.scrollIntoView({ block: 'nearest' });
  el.querySelector<HTMLElement>('button:not([disabled]), input, textarea')?.focus({ preventScroll: true });
  el.classList.add('flash');
  setTimeout(() => el.classList.remove('flash'), 1500);
}
