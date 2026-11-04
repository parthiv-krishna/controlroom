// The tasks board: columns, sort keys for moves, the roll-up of linked sessions, and the suggested
// column move (shown, never applied automatically).
import type { FeedSession, Priority, Task, TaskStatus } from '../protocol';
import type { Ctx } from './projects';
import { attentionOf } from './status';
import { unavailable } from './workspaces';

export const TASK_COLUMNS: { id: TaskStatus; label: string; icon: string }[] = [
  { id: 'backlog', label: 'Backlog', icon: '◌' },
  { id: 'todo', label: 'Up next', icon: '○' },
  { id: 'doing', label: 'In progress', icon: '◐' },
  { id: 'review', label: 'Review', icon: '◑' },
  { id: 'done', label: 'Done', icon: '●' },
];
export const PRIORITIES: Priority[] = ['p0', 'p1', 'p2', 'p3'];
export const columnLabel = (s: TaskStatus): string => TASK_COLUMNS.find((c) => c.id === s)?.label ?? s;

/** A column's tasks in board order. */
export const columnTasks = (tasks: readonly Task[], status: TaskStatus): Task[] =>
  tasks.filter((t) => t.status === status).sort((a, b) => a.order - b.order);

/**
 * The sort key that puts a task before `before` in `column` (at the end when `before` is null).
 * Fractional: halfway between the neighbors, so only the moved task changes.
 */
export function orderFor(column: readonly Task[], movingId: string, before: string | null): number {
  const rest = column.filter((t) => t.id !== movingId);
  const i = before ? rest.findIndex((t) => t.id === before) : -1;
  if (i < 0) return rest.length ? rest[rest.length - 1].order + 1 : 1;
  const next = rest[i].order;
  const prev = i > 0 ? rest[i - 1].order : next - 2;
  return (prev + next) / 2;
}

export interface Rollup {
  approvals: number;
  questions: number;
  problems: number;
  unread: number;
  working: number;
  /** Open sessions with a workspace on a machine that can't be reached. */
  unreachable: number;
  /** One per link; null marks a missing session (deleted). */
  sessions: (FeedSession | null)[];
}

export function rollup(ctx: Ctx, t: Task): Rollup {
  const r: Rollup = { approvals: 0, questions: 0, problems: 0, unread: 0, working: 0, unreachable: 0, sessions: [] };
  for (const id of t.links) {
    const s = ctx.sessions.find((x) => x.id === id) ?? null;
    r.sessions.push(s);
    if (!s) continue;
    const a = attentionOf(s);
    if (a === 'approval') r.approvals++;
    else if (a === 'question') r.questions++;
    else if (a === 'problem') r.problems++;
    else if (a === 'unread') r.unread++;
    if (s.state !== 'closed' && unavailable(s).length) r.unreachable++;
    if (s.state === 'working' || s.state === 'starting') r.working++;
  }
  return r;
}

export function suggestMove(ctx: Ctx, t: Task): { to: TaskStatus; why: string } | null {
  const r = rollup(ctx, t);
  if ((t.status === 'backlog' || t.status === 'todo') && (r.working || r.approvals)) return { to: 'doing', why: 'a linked agent is active' };
  const linked = r.sessions.filter((s): s is FeedSession => s !== null);
  const stopped = (s: FeedSession) => s.state === 'idle' || s.state === 'closed' || s.state === 'dormant';
  if (t.status === 'doing' && linked.length && !r.working && !r.approvals && !r.problems && linked.every(stopped)) {
    return { to: 'review', why: 'all linked agents have stopped' };
  }
  return null;
}

/** The first message for "start a session for this task". */
export function taskPrompt(t: Task): string {
  const notes = t.notes.trim() ? `\n\n${t.notes.trim()}` : '';
  const list = t.checklist.length ? `\n\nChecklist:\n${t.checklist.map((c) => `- [${c.done ? 'x' : ' '}] ${c.text}`).join('\n')}` : '';
  return t.title + notes + list;
}
