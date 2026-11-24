// What the notifications panel lists for a scope: Needs you (blocking requests), Agent notices
// (the agent's open warnings and errors), Questions (non-blocking), Failed or stopped, Finished,
// plus machines the user can't reach that open sessions have workspaces on.
// A subagent's request or notice is rolled up into its parent's `requests` or `notices` (with
// `origin_child`), so only top-level sessions are read, and each appears once.
import type { FeedSession, Machine, OpenNotice, Request } from '../protocol';
import { inScope, type Ctx, type Scope } from './projects';
import { attentionOf, isLive } from './status';
import { touches } from './workspaces';

export interface RequestNote {
  s: FeedSession;
  req: Request;
}

export interface NoticeNote {
  s: FeedSession;
  notice: OpenNotice;
}

export interface Notes {
  blocking: RequestNote[];
  /** Newest first. */
  notices: NoticeNote[];
  questions: RequestNote[];
  problems: FeedSession[];
  finished: FeedSession[];
  /** Machines that can't be reached, with the open sessions that have a workspace there. */
  offline: { machine: Machine; sessions: number }[];
  total: number;
}

const isTopLevel = (ctx: Ctx, s: FeedSession): boolean => !s.parent_id || !ctx.sessions.some((p) => p.id === s.parent_id);

export function notes(ctx: Ctx, scope: Scope): Notes {
  const mine = ctx.sessions.filter((s) => s.state !== 'closed' && inScope(s, scope));
  const top = mine.filter((s) => isTopLevel(ctx, s));
  const reqs = top
    .flatMap((s) => s.requests.filter((req) => req.state === 'pending').map((req) => ({ s, req })))
    .sort((a, b) => b.req.created_at - a.req.created_at);
  const notices = top.flatMap((s) => s.notices.map((notice) => ({ s, notice }))).sort((a, b) => b.notice.created_at - a.notice.created_at);
  const recent = (a: FeedSession, b: FeedSession) => b.updated_at - a.updated_at;
  const blocking = reqs.filter((n) => n.req.blocking);
  const questions = reqs.filter((n) => !n.req.blocking);
  const problems = mine.filter((s) => attentionOf(s) === 'problem').sort(recent);
  const finished = mine.filter((s) => attentionOf(s) === 'unread').sort(recent);
  const offline = ctx.machineList
    .filter((m) => !isLive(m))
    .map((machine) => ({ machine, sessions: mine.filter((s) => touches(s, machine.id)).length }))
    .filter((x) => x.sessions > 0);
  return { blocking, notices, questions, problems, finished, offline, total: reqs.length + notices.length + problems.length + finished.length };
}
