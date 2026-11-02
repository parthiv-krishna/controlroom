// Where "start fresh" dividers go in a timeline: before the first item of each agent session that
// was started fresh, or after the last item while the new agent session has none yet.
import type { AgentSession } from '../protocol';

export interface FreshDividers {
  /** Fresh agent sessions by the id of the first item that belongs to them. */
  before: Map<string, AgentSession>;
  /** Fresh agent sessions with no item yet (shown after the last item). */
  trailing: AgentSession[];
}

/** `items` in display order; an item belongs to the latest agent session started at or before it. */
export function freshDividers(items: readonly { id: string; ts: number }[], sessions: readonly AgentSession[]): FreshDividers {
  const out: FreshDividers = { before: new Map(), trailing: [] };
  for (const s of sessions) {
    if (s.started_by !== 'fresh') continue;
    const first = items.find((it) => it.ts >= s.started_at);
    if (!first) out.trailing.push(s);
    else if (!out.before.has(first.id)) out.before.set(first.id, s);
  }
  return out;
}
