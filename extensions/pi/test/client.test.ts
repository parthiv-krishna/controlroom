import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { AgentClient, ControlroomError } from '../src/client.ts';
import { FakeDaemon, todo } from './fake-daemon.ts';

let daemon: FakeDaemon;
before(async () => {
  daemon = await FakeDaemon.start();
});
after(() => daemon.stop());

const client = (token = daemon.token) =>
  new AgentClient({ socket: daemon.socket, token, clientName: 'test', callTimeoutMs: 5_000, retryMs: { first: 5, max: 20 } });

test('calls wait for the hello and get their results', async () => {
  daemon.handlers.set('todo.list', () => ({ result: { todos: [todo('T1', 'write tests')] } }));
  const c = client();
  c.start();
  const list = await c.call('todo.list', {});
  assert.equal(list.todos[0].text, 'write tests');
  assert.ok(c.connected);
  c.close();
});

test('daemon errors keep their kind', async () => {
  daemon.handlers.set('question.withdraw', () => ({ error: { code: -32007, message: 'no request q', data: { kind: 'not_found', resource: 'request', id: 'q' } } }));
  const c = client();
  c.start();
  await assert.rejects(c.call('question.withdraw', { call_id: 'c1', question_id: 'q' }), (e: ControlroomError) => e.kind === 'not_found' && /no request q/.test(e.message));
  c.close();
});

test('a wrong token fails every call for good', async () => {
  const c = client('b'.repeat(64));
  c.start();
  await assert.rejects(c.call('todo.list', {}), (e: ControlroomError) => e.kind === 'unauthenticated');
  await assert.rejects(c.call('todo.list', {}), (e: ControlroomError) => e.kind === 'unauthenticated');
  c.close();
});

test('a call in flight survives a dropped connection and is sent again', async () => {
  const c = client();
  c.start();
  await c.call('todo.list', {});
  const hellos = daemon.hellos;
  daemon.silent.add('todo.add');
  const added = c.call('todo.add', { call_id: 'call-7', texts: ['a'] });
  // The daemon got the call but died before answering.
  while (!daemon.calls.some((x) => x.method === 'todo.add')) await new Promise((r) => setImmediate(r));
  daemon.silent.delete('todo.add');
  daemon.handlers.set('todo.add', () => ({ result: { todos: [todo('T2', 'a')] } }));
  daemon.drop();
  await daemon.helloCount(hellos + 1);
  assert.equal((await added).todos[0].id, 'T2');
  // Both attempts carry the same idempotency key.
  const keys = daemon.calls.filter((x) => x.method === 'todo.add').map((x) => x.params.call_id);
  assert.deepEqual(keys, ['call-7', 'call-7']);
  c.close();
});

test('state pushes reach listeners', async () => {
  const c = client();
  const seen: number[] = [];
  c.onState((s) => seen.push(s.todos.length));
  c.start();
  await c.call('todo.list', {});
  daemon.push({ todos: [todo('T1', 'x'), todo('T2', 'y')], questions: [], auto_continue: { status: 'on', reason: null, runs_used: 0, budget: 10 } });
  while (!seen.length) await new Promise((r) => setImmediate(r));
  assert.deepEqual(seen, [2]);
  assert.equal(c.state?.todos.length, 2);
  c.close();
});

test('an unreachable daemon times calls out with a clear error', async () => {
  const c = new AgentClient({ socket: '/nonexistent/agent.sock', token: 'x', clientName: 'test', callTimeoutMs: 50, retryMs: { first: 5, max: 10 } });
  c.start();
  await assert.rejects(c.call('todo.list', {}), (e: ControlroomError) => e.kind === 'timeout' && /not reachable/.test(e.message));
  c.close();
});
