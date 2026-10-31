// Group by Task (default), Location (the nested tree), State, Host or None.
import type { Machine, MachineSession, Project, Task } from '../protocol';
import { locTree, sortedKids, type LocNode } from './location';
import { linksSession, taskIn, type Ctx } from './projects';
import { attentionOf, isLive } from './status';

export type GroupBy = 'task' | 'location' | 'state' | 'machine' | 'none';
export const GROUPS: [GroupBy, string][] = [['task', 'Task'], ['location', 'Location'], ['state', 'State'], ['machine', 'Host'], ['none', 'None']];

/** Where "new session here" starts: a machine and a working directory. */
export interface NewAt {
  machine_id: string;
  cwd: string;
}

export interface Group {
  key: string;
  /** null for the single group of Group by None. */
  label: string | null;
  items: MachineSession[];
  task?: Task;
  machine?: Machine;
  /** A directory node of the location tree, with its full `host:path`. */
  path?: string;
  /** Nesting depth (location tree only). */
  depth?: number;
  newAt?: NewAt;
}

/** Task groups show these columns, in this order. */
const TASK_ORDER: Partial<Record<Task['status'], number>> = { doing: 0, review: 1, todo: 2 };

export function groupSessions(ctx: Ctx, p: Project | null, list: MachineSession[], by: GroupBy, collapsed: Record<string, boolean>, filtering: boolean): Group[] {
  switch (by) {
    case 'task': return byTask(ctx, p, list, filtering);
    case 'location': return byLocation(ctx, list, collapsed);
    case 'state': return byState(list);
    case 'machine': return byMachine(ctx, list);
    default: return [{ key: 'all', label: null, items: list }];
  }
}

function byTask(ctx: Ctx, p: Project | null, list: MachineSession[], filtering: boolean): Group[] {
  const tasks = ctx.tasks
    .filter((t) => taskIn(t, p) && TASK_ORDER[t.status] !== undefined)
    .sort((a, b) => (TASK_ORDER[a.status] ?? 0) - (TASK_ORDER[b.status] ?? 0) || a.order - b.order);
  const out: Group[] = [];
  for (const t of tasks) {
    const items = list.filter((s) => linksSession(t, s));
    // Unstarted tasks show as empty groups ("Not started · Start") unless a filter is active.
    if (items.length || !filtering) out.push({ key: 't:' + t.id, label: t.title, task: t, items });
  }
  const rest = list.filter((s) => !tasks.some((t) => linksSession(t, s)));
  if (rest.length) out.push({ key: 'none', label: 'Not on a task', items: rest });
  return out;
}

function byLocation(ctx: Ctx, list: MachineSession[], collapsed: Record<string, boolean>): Group[] {
  const out: Group[] = [];
  for (const { machine: m, root } of locTree(list, ctx.machineList)) {
    const live = isLive(m);
    const hostKey = 'h:' + m.id;
    out.push({ key: hostKey, label: m.name, machine: m, depth: 0, items: root.sessions, newAt: live ? { machine_id: m.id, cwd: '~' } : undefined });
    if (collapsed[hostKey]) continue;
    const walk = (n: LocNode, depth: number) => {
      for (const k of sortedKids(n)) {
        const key = `l:${m.id}:${k.path}`;
        out.push({ key, label: k.label, path: `${m.name}:${k.path}`, depth, items: k.sessions, newAt: live ? { machine_id: m.id, cwd: k.path } : undefined });
        if (!collapsed[key]) walk(k, depth + 1);
      }
    };
    walk(root, 1);
  }
  return out;
}

function byState(list: MachineSession[]): Group[] {
  const buckets: [string, string, (s: MachineSession) => boolean][] = [
    ['approval', 'Needs a decision', (s) => attentionOf(s) === 'approval'],
    ['problem', 'Failed or stopped', (s) => attentionOf(s) === 'problem'],
    ['question', 'Open questions', (s) => attentionOf(s) === 'question'],
    ['unread', 'Unread results', (s) => attentionOf(s) === 'unread'],
    ['working', 'Working', (s) => !attentionOf(s) && !s.stale && (s.state === 'working' || s.state === 'starting')],
    ['idle', 'Idle', (s) => !attentionOf(s) && !s.stale && s.state !== 'working' && s.state !== 'starting'],
    ['offline', 'Disconnected', (s) => s.stale],
  ];
  return buckets
    .map(([key, label, test]) => ({ key: 's:' + key, label, items: list.filter(test) }))
    .filter((g) => g.items.length);
}

function byMachine(ctx: Ctx, list: MachineSession[]): Group[] {
  return ctx.machineList
    .map((m) => ({
      key: 'm:' + m.id,
      label: m.name,
      machine: m,
      items: list.filter((s) => s.machine_id === m.id),
      newAt: isLive(m) ? { machine_id: m.id, cwd: '~' } : undefined,
    }))
    .filter((g) => g.items.length);
}
