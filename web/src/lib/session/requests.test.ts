import { describe, expect, it } from 'vitest';
import type { FeedSession, Request } from '../protocol';
import { openRequests } from './requests';

const req = (id: string, created_at: number, state: Request['state'] = 'pending'): Request => ({
  id, created_at, state, kind: 'permission', title: id, detail: null, body: null, blocking: true, options: [], questions: [], answer: null, origin_child: null,
});
const session = (requests: Request[]) => ({ requests }) as unknown as FeedSession;
const ids = (rs: Request[]) => rs.map((r) => r.id);

describe('openRequests', () => {
  it('lists the pending requests of the summary and the timeline once, oldest first', () => {
    const timeline = new Map([['b', req('b', 2)], ['c', req('c', 1)], ['old', req('old', 0, 'answered')]]);
    expect(ids(openRequests(session([req('a', 3), req('b', 2)]), timeline))).toEqual(['c', 'b', 'a']);
  });

  it('leaves out a request the timeline already shows settled', () => {
    expect(ids(openRequests(session([req('a', 1)]), new Map([['a', req('a', 1, 'answered')]])))).toEqual([]);
  });
});
