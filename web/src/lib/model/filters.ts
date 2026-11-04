// The session list's search and Filter facets (State, Task, Machine).
import type { FeedSession } from '../protocol';
import { inScope, linksSession, tasksFor, type Ctx, type Scope } from './projects';
import { attentionOf } from './status';
import { primaryWorkspace, touches, unavailable, wsLabel } from './workspaces';

export type FacetKey = 'state' | 'task' | 'machine';
export type Filters = Record<FacetKey, string[]>;
export const FACETS: [FacetKey, string][] = [['state', 'State'], ['task', 'Task'], ['machine', 'Machine']];
export const emptyFilters = (): Filters => ({ state: [], task: [], machine: [] });
export const filterCount = (f: Filters): number => Object.values(f).reduce((n, xs) => n + xs.length, 0);

export const STATES: [string, string][] = [
  ['needs', 'Needs you'], ['working', 'Working'], ['unread', 'Unread'], ['idle', 'Idle'], ['dormant', 'Asleep'],
  ['unreachable', 'Machine unreachable'], ['notask', 'No task'], ['closed', 'Archived'],
];

function stateTest(ctx: Ctx, key: string, s: FeedSession): boolean {
  switch (key) {
    case 'needs': { const a = attentionOf(s); return a === 'approval' || a === 'problem'; }
    case 'working': return s.state === 'working' || s.state === 'starting';
    case 'unread': return s.unread && s.state !== 'closed';
    case 'idle': return s.state === 'idle';
    case 'dormant': return s.state === 'dormant';
    case 'unreachable': return s.state !== 'closed' && unavailable(s).length > 0;
    case 'notask': return !tasksFor(ctx, s).length;
    case 'closed': return s.state === 'closed';
    default: return false;
  }
}

/**
 * Whether a session passes the facets. Archived sessions show only under State › Archived, except
 * those in `keep` (archived during this visit; they stay until the user navigates away). A session
 * passes the Machine facet when one of its workspaces is on a chosen machine.
 */
export function passes(ctx: Ctx, f: Filters, s: FeedSession, keep: ReadonlySet<string>): boolean {
  const kept = keep.has(s.id);
  if (f.state.length ? !f.state.some((k) => stateTest(ctx, k, s)) && !kept : s.state === 'closed' && !kept) return false;
  if (f.task.length && !ctx.tasks.some((t) => f.task.includes(t.id) && linksSession(t, s))) return false;
  if (f.machine.length && !f.machine.some((m) => touches(s, m))) return false;
  return true;
}

/** The name the UI shows: the user's name, else the agent's title, else the primary directory. */
export function displayName(s: Pick<FeedSession, 'name' | 'title' | 'workspaces'>): string {
  if (s.name) return s.name;
  if (s.title) return s.title;
  const w = primaryWorkspace({ id: '', workspaces: s.workspaces });
  const dir = w ? (w.root ?? w.path) : '';
  return dir.split('/').filter(Boolean).pop() || dir || 'session';
}

const ORDER: Record<string, number> = { approval: 0, problem: 1, question: 2, unread: 3 };

/** The scope's sessions that pass the filters and match the search, most urgent first. */
export function visibleSessions(ctx: Ctx, scope: Scope, f: Filters, q: string, keep: ReadonlySet<string>): FeedSession[] {
  const query = q.trim().toLowerCase();
  return ctx.sessions
    .filter((s) => inScope(s, scope) && passes(ctx, f, s, keep) && (!query || haystack(ctx, s).includes(query)))
    .sort((a, b) => rank(a) - rank(b) || b.updated_at - a.updated_at);
}

const rank = (s: FeedSession): number => ORDER[attentionOf(s) ?? ''] ?? 4;

/** Name, every workspace as `machine:path`, and the linked tasks' titles. */
function haystack(ctx: Ctx, s: FeedSession): string {
  const where = s.workspaces.map((w) => wsLabel(w, ctx.machines)).join(' ');
  return `${displayName(s)} ${where} ${tasksFor(ctx, s).map((t) => t.title).join(' ')}`.toLowerCase();
}
