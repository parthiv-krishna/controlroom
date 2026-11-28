import { describe, expect, it } from 'vitest';
import { isRead, resendable } from './policy';

describe('call policy across a drop', () => {
  it('resends reads, previews and op_id commands, nothing else', () => {
    expect(resendable('models.list', {})).toBe(true);
    expect(resendable('session.prompt', { op_id: 'o1', id: 's1' })).toBe(true);
    expect(resendable('profiles.move', { profile_id: 'p', dry_run: true })).toBe(true);
    expect(resendable('profiles.move', { profile_id: 'p' })).toBe(false);
    expect(resendable('tasks.delete', { task_id: 't1' })).toBe(false);
    // Connection-scoped: the sync engine makes them again.
    expect(resendable('hello', { device_id: 'd' } as never)).toBe(false);
    expect(resendable('state.subscribe', { since: null })).toBe(false);
    expect(isRead('fs.list')).toBe(true);
    expect(isRead('session.prompt')).toBe(false);
  });
});
