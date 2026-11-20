import { describe, expect, it } from 'vitest';
import type { Todo, TodoStatus } from '../protocol';
import { todoDelta } from './todos';

const todo = (id: string, status: TodoStatus, text = id): Todo => ({ id, text, status, reason: null, updated_by: { kind: 'agent' }, updated_at: 0 }) as Todo;

describe('todoDelta', () => {
  it('counts what changed, in a fixed order', () => {
    const before = [todo('a', 'pending'), todo('b', 'in_progress'), todo('c', 'pending')];
    const after = [todo('a', 'done'), todo('b', 'done'), todo('d', 'pending'), todo('c', 'pending', 'c2')];
    expect(todoDelta(before, after)).toBe('2 done, 1 added, 1 edited');
  });
  it('names removals and a first list', () => {
    expect(todoDelta([todo('a', 'pending')], [])).toBe('1 removed');
    expect(todoDelta([], [todo('a', 'pending'), todo('b', 'pending')])).toBe('2 added');
    expect(todoDelta([todo('a', 'pending')], [todo('a', 'pending')])).toBe('updated');
  });
});
