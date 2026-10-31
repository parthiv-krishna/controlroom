// Project views over sessions and tasks, and the stoplight counts shown per project.
import type { Machine, MachineId, MachineSession, Project, Task } from '../protocol';
import { prefixMatch } from './location';
import { attentionOf } from './status';

/** The state the pure view logic works on (the store's maps and lists). */
export interface Ctx {
  machines: ReadonlyMap<MachineId, Machine>;
  machineList: readonly Machine[];
  sessions: readonly MachineSession[];
  tasks: readonly Task[];
}

export const sessionKey = (machineId: MachineId, sessionId: string): string => `${machineId}/${sessionId}`;
export const keyOf = (s: { machine_id: MachineId; id: string }): string => sessionKey(s.machine_id, s.id);

export const linksSession = (t: Task, s: MachineSession): boolean =>
  t.links.some((l) => l.machine_id === s.machine_id && l.session_id === s.id);

export const tasksFor = (ctx: Ctx, s: MachineSession): Task[] => ctx.tasks.filter((t) => linksSession(t, s));

/** A task belongs to a project only by assignment. */
export const taskIn = (t: Task, p: Project | null): boolean => !p || t.project_ids.includes(p.id);

/** A session belongs to a project at one of its locations, or through a task assigned to it. */
export function inProject(ctx: Ctx, s: MachineSession, p: Project | null): boolean {
  if (!p) return true;
  const m = ctx.machines.get(s.machine_id);
  if (m && p.location_prefixes.some((x) => prefixMatch(s, m, x))) return true;
  return ctx.tasks.some((t) => taskIn(t, p) && linksSession(t, s));
}

/** Stoplight counts: red = problems, yellow = approvals (ring = only questions), green = unread. */
export interface Health {
  approvals: number;
  questions: number;
  problems: number;
  unread: number;
}

export function health(ctx: Ctx, p: Project | null): Health {
  const h: Health = { approvals: 0, questions: 0, problems: 0, unread: 0 };
  for (const s of ctx.sessions) {
    const a = attentionOf(s);
    if (!a || !inProject(ctx, s, p)) continue;
    if (a === 'approval') h.approvals++;
    else if (a === 'question') h.questions++;
    else if (a === 'problem') h.problems++;
    else h.unread++;
  }
  return h;
}

/** Sessions needing a decision or a fix outside the project (the phone's project switcher badge). */
export const needsYouElsewhere = (ctx: Ctx, p: Project | null): number =>
  p ? ctx.sessions.filter((s) => { const a = attentionOf(s); return (a === 'approval' || a === 'problem') && !inProject(ctx, s, p); }).length : 0;

/** The two-letter code shown in the rail and on badges. */
export const projectCode = (p: Project): string => p.name.trim().slice(0, 2);
