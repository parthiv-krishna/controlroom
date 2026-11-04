// Projects are named groups of tasks: a session belongs to the projects of the tasks that link
// it (the server computes `project_ids`), else it is Unassigned. The rail scopes the views to a
// project, to Unassigned, or to everything; the stoplight counts are per scope.
import type { FeedSession, Machine, MachineId, Overlap, Project, Task } from '../protocol';
import { attentionOf } from './status';

/** The rail entry for sessions and tasks no project claims. */
export const UNASSIGNED = 'unassigned';

/** What the views show: one project, the Unassigned group, or everything (`null`). */
export type Scope = Project | typeof UNASSIGNED | null;

/** The state the pure view logic works on (the store's maps and lists). */
export interface Ctx {
  machines: ReadonlyMap<MachineId, Machine>;
  machineList: readonly Machine[];
  sessions: readonly FeedSession[];
  tasks: readonly Task[];
  overlaps: readonly Overlap[];
}

export const linksSession = (t: Task, s: { id: string }): boolean => t.links.includes(s.id);

export const tasksFor = (ctx: Ctx, s: { id: string }): Task[] => ctx.tasks.filter((t) => linksSession(t, s));

/** A task is in a project by assignment; Unassigned holds the tasks of no project. */
export function taskIn(t: Task, scope: Scope): boolean {
  if (!scope) return true;
  if (scope === UNASSIGNED) return !t.project_ids.length;
  return t.project_ids.includes(scope.id);
}

/** A session is in the projects of its linked tasks; Unassigned holds the rest. */
export function inScope(s: FeedSession, scope: Scope): boolean {
  if (!scope) return true;
  if (scope === UNASSIGNED) return !s.project_ids.length;
  return s.project_ids.includes(scope.id);
}

export const scopeName = (scope: Scope): string => (!scope ? 'All projects' : scope === UNASSIGNED ? 'Unassigned' : scope.name);

/** Stoplight counts: red = problems, yellow = approvals (ring = only questions), green = unread. */
export interface Health {
  approvals: number;
  questions: number;
  problems: number;
  unread: number;
}

export function health(ctx: Ctx, scope: Scope): Health {
  const h: Health = { approvals: 0, questions: 0, problems: 0, unread: 0 };
  for (const s of ctx.sessions) {
    const a = attentionOf(s);
    if (!a || !inScope(s, scope)) continue;
    if (a === 'approval') h.approvals++;
    else if (a === 'question') h.questions++;
    else if (a === 'problem') h.problems++;
    else h.unread++;
  }
  return h;
}

/** Sessions needing a decision or a fix outside the scope (the phone's project switcher badge). */
export const needsYouElsewhere = (ctx: Ctx, scope: Scope): number =>
  scope
    ? ctx.sessions.filter((s) => {
        const a = attentionOf(s);
        return (a === 'approval' || a === 'problem') && !inScope(s, scope);
      }).length
    : 0;

/** The two-letter code shown in the rail and on badges. */
export const projectCode = (p: Project): string => p.name.trim().slice(0, 2);
