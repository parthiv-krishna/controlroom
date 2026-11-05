// Group by Task (default), Machine (a session is listed under every machine it touches), State
// or None.
import type { FeedSession, Machine, Task } from '../protocol';
import { linksSession, taskIn, type Ctx, type Scope } from './projects';
import { attentionOf } from './status';
import { touches, unavailable } from './workspaces';

export type GroupBy = 'task' | 'machine' | 'state' | 'none';
export const GROUPS: [GroupBy, string][] = [['task', 'Task'], ['machine', 'Machine'], ['state', 'State'], ['none', 'None']];

export interface Group {
  key: string;
  /** null for the single group of Group by None. */
  label: string | null;
  items: FeedSession[];
  task?: Task;
  /** A machine group: "new session here" starts on it while it is connected. */
  machine?: Machine;
}

/** Task groups show these columns, in this order. */
const TASK_ORDER: Partial<Record<Task['status'], number>> = { doing: 0, review: 1, todo: 2 };

export function groupSessions(ctx: Ctx, scope: Scope, list: FeedSession[], by: GroupBy, filtering: boolean): Group[] {
  switch (by) {
    case 'task': return byTask(ctx, scope, list, filtering);
    case 'machine': return byMachine(ctx, list);
    case 'state': return byState(list);
    default: return [{ key: 'all', label: null, items: list }];
  }
}

function byTask(ctx: Ctx, scope: Scope, list: FeedSession[], filtering: boolean): Group[] {
  const tasks = ctx.tasks
    .filter((t) => taskIn(t, scope) && TASK_ORDER[t.status] !== undefined)
    .sort((a, b) => (TASK_ORDER[a.status] ?? 0) - (TASK_ORDER[b.status] ?? 0) || a.order - b.order);
  const out: Group[] = [];
  for (const t of tasks) {
    const items = list.filter((s) => linksSession(t, s));
    // Unstarted tasks show as empty groups ("Start") unless a filter is active.
    if (items.length || !filtering) out.push({ key: 't:' + t.id, label: t.title, task: t, items });
  }
  const rest = list.filter((s) => !tasks.some((t) => linksSession(t, s)));
  if (rest.length) out.push({ key: 'none', label: 'Not on a task', items: rest });
  return out;
}

function byMachine(ctx: Ctx, list: FeedSession[]): Group[] {
  return ctx.machineList
    .map((m) => ({ key: 'm:' + m.id, label: m.name, machine: m, items: list.filter((s) => touches(s, m.id)) }))
    .filter((g) => g.items.length);
}

function byState(list: FeedSession[]): Group[] {
  const buckets: [string, string, (s: FeedSession) => boolean][] = [
    ['approval', 'Needs a decision', (s) => attentionOf(s) === 'approval'],
    ['problem', 'Failed or stopped', (s) => attentionOf(s) === 'problem'],
    ['question', 'Open questions', (s) => attentionOf(s) === 'question'],
    ['unread', 'Unread results', (s) => attentionOf(s) === 'unread'],
    ['working', 'Working', (s) => !attentionOf(s) && (s.state === 'working' || s.state === 'starting')],
    ['idle', 'Idle', (s) => !attentionOf(s) && s.state !== 'working' && s.state !== 'starting'],
  ];
  const groups = buckets
    .map(([key, label, test]) => ({ key: 's:' + key, label, items: list.filter(test) }))
    .filter((g) => g.items.length);
  // Sessions waiting on an unreachable machine are listed again, together.
  const away = list.filter((s) => s.state !== 'closed' && unavailable(s).length);
  if (away.length) groups.push({ key: 's:unreachable', label: 'Machine unreachable', items: away });
  return groups;
}
