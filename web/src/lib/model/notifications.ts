// What the notifications panel lists for a project: Needs you (blocking requests), Questions
// (non-blocking), Failed or stopped, Finished, plus machines the user can't reach.
// A subagent's request is rolled up into its parent's `requests` (with `origin_child`), so only
// top-level sessions are read, and each request appears once.
import type { Machine, MachineSession, Project, Request } from '../protocol';
import { inProject, type Ctx } from './projects';
import { attentionOf, isLive } from './status';

export interface RequestNote {
  s: MachineSession;
  req: Request;
}

export interface Notes {
  blocking: RequestNote[];
  questions: RequestNote[];
  problems: MachineSession[];
  finished: MachineSession[];
  /** Unreachable machines with requests waiting (they don't count as "needs you"). */
  offline: { machine: Machine; waiting: number }[];
  total: number;
}

const isTopLevel = (ctx: Ctx, s: MachineSession): boolean =>
  !s.parent_id || !ctx.sessions.some((p) => p.machine_id === s.machine_id && p.id === s.parent_id);

export function notes(ctx: Ctx, p: Project | null): Notes {
  const mine = ctx.sessions.filter((s) => s.state !== 'closed' && inProject(ctx, s, p));
  const reqs = mine
    .filter((s) => !s.stale && isTopLevel(ctx, s))
    .flatMap((s) => s.requests.filter((req) => req.state === 'pending').map((req) => ({ s, req })))
    .sort((a, b) => b.req.created_at - a.req.created_at);
  const recent = (a: MachineSession, b: MachineSession) => b.updated_at - a.updated_at;
  const blocking = reqs.filter((n) => n.req.blocking);
  const questions = reqs.filter((n) => !n.req.blocking);
  const problems = mine.filter((s) => attentionOf(s) === 'problem').sort(recent);
  const finished = mine.filter((s) => attentionOf(s) === 'unread').sort(recent);
  const offline = ctx.machineList
    .filter((m) => !isLive(m))
    .map((machine) => ({ machine, waiting: mine.filter((s) => s.machine_id === machine.id && s.stale).reduce((n, s) => n + s.requests.length, 0) }))
    .filter((x) => x.waiting > 0);
  return { blocking, questions, problems, finished, offline, total: reqs.length + problems.length + finished.length };
}
