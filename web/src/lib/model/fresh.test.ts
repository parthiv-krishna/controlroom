import { describe, expect, it } from 'vitest';
import type { AgentSession, StartedBy } from '../protocol';
import { freshDividers } from './fresh';

const agent = (started_at: number, started_by: StartedBy): AgentSession => ({ driver: 'pi', native_id: `n${started_at}`, started_at, ended_at: null, started_by });
const items = [
  { id: 'a', ts: 100 },
  { id: 'b', ts: 200 },
  { id: 'c', ts: 300 },
];

describe('freshDividers', () => {
  it('puts a divider before the first item of each fresh agent session', () => {
    const d = freshDividers(items, [agent(50, 'new'), agent(150, 'fresh'), agent(250, 'resume')]);
    expect([...d.before.keys()]).toEqual(['b']);
    expect(d.before.get('b')?.started_at).toBe(150);
    expect(d.trailing).toEqual([]);
  });

  it('trails the last item while the fresh session has no item yet', () => {
    const d = freshDividers(items, [agent(50, 'new'), agent(150, 'fresh'), agent(400, 'fresh')]);
    expect([...d.before.keys()]).toEqual(['b']);
    expect(d.trailing.map((s) => s.started_at)).toEqual([400]);
  });

  it('shows one divider when fresh sessions share their first item', () => {
    const d = freshDividers(items, [agent(110, 'fresh'), agent(120, 'fresh')]);
    expect([...d.before.entries()].map(([id, s]) => [id, s.started_at])).toEqual([['b', 110]]);
  });
});
