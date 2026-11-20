// What a change of the shared todo list did, in a few words.
import type { Todo, TodoStatus } from '../protocol';

const VERB: Partial<Record<TodoStatus, string>> = { done: 'done', in_progress: 'started', blocked: 'blocked', skipped: 'skipped' };

/** "2 done, 1 added" (counts by what changed, in a fixed order); "updated" when nothing counted. */
export function todoDelta(before: readonly Todo[], after: readonly Todo[]): string {
  const old = new Map(before.map((t) => [t.id, t]));
  const ids = new Set(after.map((t) => t.id));
  const counts = new Map<string, number>();
  const add = (k: string) => counts.set(k, (counts.get(k) ?? 0) + 1);
  for (const t of after) {
    const was = old.get(t.id);
    if (!was) add('added');
    else if (was.status !== t.status) add(VERB[t.status] ?? 'reopened');
    else if (was.text !== t.text) add('edited');
  }
  for (const t of before) if (!ids.has(t.id)) add('removed');
  const ORDER = ['done', 'started', 'blocked', 'skipped', 'reopened', 'added', 'edited', 'removed'];
  const parts = ORDER.filter((k) => counts.has(k)).map((k) => `${counts.get(k)} ${k}`);
  return parts.length ? parts.join(', ') : 'updated';
}
