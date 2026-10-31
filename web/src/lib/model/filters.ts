// The session list's search and Filter facets (State, Task, Location, Host, Agent).
import type { MachineSession, Project } from '../protocol';
import { loc, underLoc } from './location';
import { inProject, keyOf, linksSession, tasksFor, type Ctx } from './projects';
import { attentionOf } from './status';

export type FacetKey = 'state' | 'task' | 'location' | 'machine' | 'agent';
export type Filters = Record<FacetKey, string[]>;
export const FACETS: [FacetKey, string][] = [['state', 'State'], ['task', 'Task'], ['location', 'Location'], ['machine', 'Host'], ['agent', 'Agent']];
export const emptyFilters = (): Filters => ({ state: [], task: [], location: [], machine: [], agent: [] });
export const filterCount = (f: Filters): number => Object.values(f).reduce((n, xs) => n + xs.length, 0);

export const STATES: [string, string][] = [
  ['needs', 'Needs you'], ['working', 'Working'], ['unread', 'Unread'], ['idle', 'Idle'], ['dormant', 'Asleep'],
  ['offline', 'Disconnected'], ['notask', 'No task'], ['closed', 'Archived'],
];

function stateTest(ctx: Ctx, key: string, s: MachineSession): boolean {
  const live = !s.stale;
  switch (key) {
    case 'needs': { const a = attentionOf(s); return a === 'approval' || a === 'problem'; }
    case 'working': return live && (s.state === 'working' || s.state === 'starting');
    case 'unread': return s.unread && s.state !== 'closed';
    case 'idle': return live && s.state === 'idle';
    case 'dormant': return live && s.state === 'dormant';
    case 'offline': return s.stale && s.state !== 'closed';
    case 'notask': return !tasksFor(ctx, s).length;
    case 'closed': return s.state === 'closed';
    default: return false;
  }
}

/**
 * Whether a session passes the facets. Archived sessions show only under State › Archived, except
 * those in `keep` (archived during this visit; they stay until the user navigates away).
 */
export function passes(ctx: Ctx, f: Filters, s: MachineSession, keep: ReadonlySet<string>): boolean {
  const kept = keep.has(keyOf(s));
  if (f.state.length ? !f.state.some((k) => stateTest(ctx, k, s)) && !kept : s.state === 'closed' && !kept) return false;
  if (f.task.length && !ctx.tasks.some((t) => f.task.includes(t.id) && linksSession(t, s))) return false;
  const m = ctx.machines.get(s.machine_id);
  if (f.location.length && (!m || !f.location.some((x) => underLoc(loc(s, m), x)))) return false;
  if (f.machine.length && !f.machine.includes(s.machine_id)) return false;
  if (f.agent.length && !f.agent.includes(s.agent)) return false;
  return true;
}

/** The name the UI shows: the user's name, else the agent's title, else the directory. */
export const displayName = (s: { name: string | null; title: string | null; cwd: string }): string =>
  s.name || s.title || s.cwd.split('/').filter(Boolean).pop() || s.cwd;

const ORDER: Record<string, number> = { approval: 0, problem: 1, question: 2, unread: 3 };

/** The project's sessions that pass the filters and match the search, most urgent first. */
export function visibleSessions(ctx: Ctx, p: Project | null, f: Filters, q: string, keep: ReadonlySet<string>): MachineSession[] {
  const query = q.trim().toLowerCase();
  return ctx.sessions
    .filter((s) => inProject(ctx, s, p) && passes(ctx, f, s, keep) && (!query || haystack(ctx, s).includes(query)))
    .sort((a, b) => rank(a) - rank(b) || b.updated_at - a.updated_at);
}

const rank = (s: MachineSession): number => ORDER[attentionOf(s) ?? ''] ?? 4;

function haystack(ctx: Ctx, s: MachineSession): string {
  const m = ctx.machines.get(s.machine_id);
  return `${displayName(s)} ${m ? loc(s, m) : s.cwd} ${tasksFor(ctx, s).map((t) => t.title).join(' ')}`.toLowerCase();
}
