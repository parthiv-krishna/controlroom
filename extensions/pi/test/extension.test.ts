import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { activate, type PiApi, type PiContext, type PiTool, widget } from '../src/extension.ts';
import { SECTION } from '../src/prompt.ts';
import { renderTodos } from '../src/tools.ts';
import type { TodoStatus } from '../src/protocol/TodoStatus';
import { FakeDaemon, todo } from './fake-daemon.ts';

/** A pi stand-in that records registrations and lets the test fire events. */
class FakePi implements PiApi {
  tools = new Map<string, PiTool>();
  handlers = new Map<string, ((event: never, ctx: PiContext) => void)[]>();
  registerTool(tool: PiTool) {
    this.tools.set(tool.name, tool);
  }
  on(event: string, handler: (event: never, ctx: PiContext) => void) {
    this.handlers.set(event, [...(this.handlers.get(event) ?? []), handler]);
    return () => {};
  }
  fire(event: string, payload: object, ctx: PiContext = { mode: 'rpc' }) {
    for (const h of this.handlers.get(event) ?? []) h(payload as never, ctx);
  }
  async run(name: string, params: object, callId = 'call-1') {
    const result = await this.tools.get(name)!.execute(callId, params);
    return { text: result.content[0].text, details: result.details as Record<string, unknown> };
  }
}

let daemon: FakeDaemon;
before(async () => {
  daemon = await FakeDaemon.start();
});
after(() => daemon.stop());

const env = () => ({ CONTROLROOM_SESSION_ID: 'S', CONTROLROOM_AGENT_SOCKET: daemon.socket, CONTROLROOM_AGENT_TOKEN: daemon.token });

test('outside controlroom nothing is registered', () => {
  const pi = new FakePi();
  assert.equal(activate(pi, {}), null);
  assert.equal(pi.tools.size, 0);
  assert.equal(pi.handlers.size, 0);
});

test('registers the tools and adds the prompt section', () => {
  const pi = new FakePi();
  const client = activate(pi, env())!;
  assert.deepEqual([...pi.tools.keys()], ['ask_user', 'todo', 'notify_user']);
  const event = { systemPromptOptions: { sections: { other: 'x' } as Record<string, string> } };
  pi.fire('before_agent_start', event);
  assert.equal(event.systemPromptOptions.sections.other, 'x');
  assert.match(event.systemPromptOptions.sections[SECTION], /ask_user/);
  assert.match(event.systemPromptOptions.sections[SECTION], /Auto-continue/);
  client.close();
});

test('ask_user sends the questions and returns the id and the reminder', async () => {
  daemon.handlers.set('question.ask', () => ({ result: { question_id: 'question-1', reminder: 'Question sent; keep working.' } }));
  const pi = new FakePi();
  activate(pi, env());
  pi.fire('session_start', {});
  const out = await pi.run('ask_user', { questions: [{ question: 'Which DB?', options: ['Postgres', { label: 'SQLite', description: 'embedded' }] }] }, 'tc-1');
  assert.equal(out.text, 'Question question-1 sent. Question sent; keep working.');
  const sent = daemon.calls.findLast((c) => c.method === 'question.ask')!.params;
  assert.deepEqual(sent, {
    call_id: 'tc-1',
    questions: [{ text: 'Which DB?', options: [{ label: 'Postgres' }, { label: 'SQLite', description: 'embedded' }] }],
  });
  pi.fire('session_shutdown', {});
});

test('ask_user withdraws, and reports an earlier answer', async () => {
  daemon.handlers.set('question.withdraw', (p) =>
    p.question_id === 'q-open' ? { result: { state: 'withdrawn', answer: null } } : { result: { state: 'answered', answer: { label: 'Postgres' } } },
  );
  const pi = new FakePi();
  activate(pi, env());
  pi.fire('session_start', {});
  assert.equal((await pi.run('ask_user', { withdraw: 'q-open' })).text, 'Question q-open withdrawn.');
  assert.match((await pi.run('ask_user', { withdraw: 'q-done' })).text, /already answered \(answer: Postgres\)/);
  await assert.rejects(pi.run('ask_user', {}), /at least one question/);
  pi.fire('session_shutdown', {});
});

test('todo actions map onto the agent API, numbers resolve to ids', async () => {
  let list = [todo('T1', 'write tests'), todo('T2', 'ship')];
  daemon.handlers.set('todo.list', () => ({ result: { todos: list } }));
  daemon.handlers.set('todo.add', (p) => {
    list = [...list, ...(p.texts as string[]).map((t, i) => todo(`N${i}`, t))];
    return { result: { todos: list } };
  });
  daemon.handlers.set('todo.update', (p) => {
    list = list.map((t) => (t.id === p.todo_id ? { ...t, status: (p.status as TodoStatus) ?? t.status, reason: (p.reason as string) ?? null } : t));
    return { result: { todos: list } };
  });
  const pi = new FakePi();
  activate(pi, env());
  pi.fire('session_start', {});
  assert.match((await pi.run('todo', { action: 'add', items: ['docs'] })).text, /3\. \[ \] docs/);
  await pi.run('todo', { action: 'complete', id: 2 }, 'tc-2');
  assert.deepEqual(daemon.calls.findLast((c) => c.method === 'todo.update')!.params, { call_id: 'tc-2', todo_id: 'T2', status: 'done' });
  await pi.run('todo', { action: 'block', id: 'T1', reason: 'needs a key' });
  assert.equal(list[0].status, 'blocked');
  await assert.rejects(pi.run('todo', { action: 'block', id: 'T1' }), /needs a reason/);
  await assert.rejects(pi.run('todo', { action: 'complete', id: 9 }), /no todo #9/);
  const listed = await pi.run('todo', { action: 'list' });
  assert.equal(listed.text, renderTodos(list));
  pi.fire('session_shutdown', {});
});

test('notify_user posts a notice', async () => {
  daemon.handlers.set('notify', () => ({ result: {} }));
  const pi = new FakePi();
  activate(pi, env());
  pi.fire('session_start', {});
  assert.equal((await pi.run('notify_user', { text: 'Deployed to staging', level: 'warning' }, 'tc-n')).text, 'Notice shown to the user.');
  assert.deepEqual(daemon.calls.findLast((c) => c.method === 'notify')!.params, { call_id: 'tc-n', text: 'Deployed to staging', level: 'warning' });
  pi.fire('session_shutdown', {});
});

test('the terminal widget summarizes the state', () => {
  const ac = { status: 'stalled' as const, reason: '3 runs without progress', runs_used: 4, budget: 10 };
  assert.equal(widget({ todos: [], questions: [], auto_continue: { ...ac, status: 'on' } }), undefined);
  assert.deepEqual(widget({ todos: [todo('a', 'x', 'done'), todo('b', 'y'), todo('c', 'z', 'skipped')], questions: [{} as never], auto_continue: ac }), [
    'controlroom: todo 1/2 · 1 open question · auto-continue stalled',
  ]);
});
