import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { activate, type BeforeAgentStart, widget } from '../src/extension.ts';
import { SECTION } from '../src/prompt.ts';
import type { SessionInfo } from '../src/protocol/SessionInfo.ts';
import type { TodoStatus } from '../src/protocol/TodoStatus.ts';
import { CWD, type PromptSkill, WORKSPACES_SECTION } from '../src/remote/prompt.ts';
import { renderTodos } from '../src/tools.ts';
import { FakeDaemon, ONE, shell, TWO, todo, workspace } from './fake-daemon.ts';
import { FakePi, started } from './fake-pi.ts';

let daemon: FakeDaemon;
before(async () => {
  daemon = await FakeDaemon.start();
});
after(() => daemon.stop());

/** Fires `before_agent_start` and returns the prompt options as pi would use them. */
async function prompt(pi: FakePi) {
  const event = {
    systemPromptOptions: { sections: { other: 'x' } as Record<string, string>, cwd: '/var/lib/agent/scratch', contextFiles: [{ path: '/x', content: 'local' }], skills: [] as PromptSkill[] },
  } satisfies BeforeAgentStart;
  await pi.fire('before_agent_start', event);
  return event.systemPromptOptions;
}

test('outside controlroom nothing is registered', () => {
  const pi = new FakePi();
  assert.equal(activate(pi, {}), null);
  assert.equal(pi.tools.size, 0);
  assert.equal(pi.handlers.size, 0);
});

test("registers pi's file and shell tools by name, then controlroom's own", async () => {
  const pi = await started(daemon, ONE);
  assert.deepEqual(
    [...pi.tools.keys()],
    ['read', 'write', 'edit', 'bash', 'shell_new', 'shell_list', 'grep', 'find', 'ls', 'job', 'ask_user', 'todo', 'notify_user'],
  );
  // pi's edit argument shim (a single edit, edits as a JSON string) is kept.
  assert.deepEqual(pi.tools.get('edit')!.prepareArguments!({ path: 'a', oldText: 'x', newText: 'y' }), { path: 'a', edits: [{ oldText: 'x', newText: 'y' }] });
  await pi.fire('session_shutdown', {});
});

test('the system prompt carries the workspaces, their instruction files and skills', async () => {
  const info: SessionInfo = {
    workspaces: [
      workspace('devbox', 'src/api', {
        primary: true,
        context_files: [
          { path: '/home/alice/src/AGENTS.md', content: 'parent rules' },
          { path: '/home/alice/src/api/AGENTS.md', content: 'api rules' },
        ],
        skills: [{ name: 'deploy', description: 'Deploy the API', path: '/home/alice/src/api/.pi/skills/deploy/SKILL.md' }],
      }),
      workspace('devbox', 'src/web', {
        shell: 'devbox-web',
        context_files: [{ path: '/home/alice/src/AGENTS.md', content: 'parent rules' }],
      }),
      workspace('buildbox', 'work/web', {
        availability: { connectivity: { state: 'unreachable' }, since: Date.UTC(2026, 0, 2, 3, 4, 5) },
        context_files: [{ path: '/srv/AGENTS.md', content: 'build rules' }],
      }),
    ],
    shells: [shell('devbox', 'devbox', '/home/alice/src/api/src'), shell('buildbox', 'buildbox', '/tmp')],
  };
  const pi = await started(daemon, info);
  const options = await prompt(pi);
  assert.equal(options.cwd, CWD);
  assert.deepEqual(options.contextFiles, [
    { path: 'devbox:~/src/AGENTS.md', content: 'parent rules' },
    { path: 'devbox:~/src/api/AGENTS.md', content: 'api rules' },
    { path: 'buildbox:/srv/AGENTS.md', content: 'build rules' },
  ]);
  assert.equal(options.skills.length, 1);
  assert.equal(options.skills[0].name, 'deploy');
  assert.equal(options.skills[0].filePath, 'devbox:~/src/api/.pi/skills/deploy/SKILL.md');
  assert.equal(options.sections.other, 'x');
  assert.match(options.sections[SECTION], /ask_user/);
  const section = options.sections[WORKSPACES_SECTION];
  assert.match(section, /Every file path is machine:path, e\.g\. devbox:~\/src\/api\/README\.md/);
  assert.match(section, /several workspaces, so always name the machine/);
  assert.match(section, /^- devbox:~\/src\/api \(primary; root devbox:~\/src\/api; default shell "devbox"\)$/m);
  assert.match(section, /^- devbox:~\/src\/web \(root devbox:~\/src\/web; default shell "devbox-web"\)$/m);
  assert.match(section, /^- buildbox:~\/work\/web \(.*buildbox unreachable since 2026-01-02T03:04:05\.000Z/m);
  assert.match(section, /^- devbox: on devbox, now in devbox:~\/src\/api\/src$/m);
  assert.match(section, /^- buildbox: on buildbox, now in buildbox:\/tmp$/m);
  await pi.fire('session_shutdown', {});
});

test('a session.info notification refreshes the prompt and the tools', async () => {
  const pi = await started(daemon, ONE);
  assert.match((await prompt(pi)).sections[WORKSPACES_SECTION], /A path without a machine means devbox/);
  // Connected (the prompt fetched the info); the daemon now pushes a change.
  daemon.notify('session.info', TWO);
  for (;;) {
    const section = (await prompt(pi)).sections[WORKSPACES_SECTION];
    if (section.includes('buildbox')) break;
    await new Promise((resolve) => setImmediate(resolve));
  }
  // The bare path that worked with one workspace is now refused.
  await assert.rejects(pi.run('read', { path: 'README.md' }), /"README\.md" names no machine.*devbox:README\.md/s);
  await pi.fire('session_shutdown', {});
});

test('without session info the prompt says so and keeps the controlroom section', async () => {
  daemon.handlers.set('session.info', () => ({ error: { code: -32603, message: 'boom', data: { kind: 'internal_error' } } }));
  const pi = await started(daemon);
  const options = await prompt(pi);
  assert.match(options.sections[WORKSPACES_SECTION], /could not be loaded/);
  assert.match(options.sections[SECTION], /Auto-continue/);
  assert.equal(options.cwd, CWD);
  await pi.fire('session_shutdown', {});
});

test('ask_user sends the questions and returns the id and the reminder', async () => {
  daemon.handlers.set('question.ask', () => ({ result: { question_id: 'question-1', reminder: 'Question sent; keep working.' } }));
  const pi = await started(daemon);
  const out = await pi.run('ask_user', { questions: [{ question: 'Which DB?', options: ['Postgres', { label: 'SQLite', description: 'embedded' }] }] }, { callId: 'tc-1' });
  assert.equal(out.text, 'Question question-1 sent. Question sent; keep working.');
  assert.deepEqual(daemon.paramsOf('question.ask').at(-1), {
    call_id: 'tc-1',
    questions: [{ text: 'Which DB?', options: [{ label: 'Postgres' }, { label: 'SQLite', description: 'embedded' }] }],
  });
  await pi.fire('session_shutdown', {});
});

test('ask_user withdraws, and reports an earlier answer', async () => {
  daemon.handlers.set('question.withdraw', (p) =>
    p.question_id === 'q-open' ? { result: { state: 'withdrawn', answer: null } } : { result: { state: 'answered', answer: { label: 'Postgres' } } },
  );
  const pi = await started(daemon);
  assert.equal((await pi.run('ask_user', { withdraw: 'q-open' })).text, 'Question q-open withdrawn.');
  assert.match((await pi.run('ask_user', { withdraw: 'q-done' })).text, /already answered \(answer: Postgres\)/);
  await assert.rejects(pi.run('ask_user', {}), /at least one question/);
  await pi.fire('session_shutdown', {});
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
  const pi = await started(daemon);
  assert.match((await pi.run('todo', { action: 'add', items: ['docs'] })).text, /3\. \[ \] docs/);
  await pi.run('todo', { action: 'complete', id: 2 }, { callId: 'tc-2' });
  assert.deepEqual(daemon.paramsOf('todo.update').at(-1), { call_id: 'tc-2', todo_id: 'T2', status: 'done' });
  await pi.run('todo', { action: 'block', id: 'T1', reason: 'needs a key' });
  assert.equal(list[0].status, 'blocked');
  await assert.rejects(pi.run('todo', { action: 'block', id: 'T1' }), /needs a reason/);
  await assert.rejects(pi.run('todo', { action: 'complete', id: 9 }), /no todo #9/);
  const listed = await pi.run('todo', { action: 'list' });
  assert.equal(listed.text, renderTodos(list));
  await pi.fire('session_shutdown', {});
});

test('notify_user posts a notice', async () => {
  daemon.handlers.set('notify', () => ({ result: {} }));
  const pi = await started(daemon);
  assert.equal((await pi.run('notify_user', { text: 'Deployed to staging', level: 'warning' }, { callId: 'tc-n' })).text, 'Notice shown to the user.');
  assert.deepEqual(daemon.paramsOf('notify').at(-1), { call_id: 'tc-n', text: 'Deployed to staging', level: 'warning' });
  await pi.fire('session_shutdown', {});
});

test('the terminal widget summarizes the state', () => {
  const ac = { status: 'stalled' as const, reason: '3 runs without progress', runs_used: 4, budget: 10 };
  assert.equal(widget({ todos: [], questions: [], auto_continue: { ...ac, status: 'on' } }), undefined);
  assert.deepEqual(widget({ todos: [todo('a', 'x', 'done'), todo('b', 'y'), todo('c', 'z', 'skipped')], questions: [{} as never], auto_continue: ac }), [
    'controlroom: todo 1/2 · 1 open question · auto-continue stalled',
  ]);
});

test('job runs on a machine, follows, reports status and kills through the agent API', async () => {
  const running = {
    id: 'J1',
    name: 'tests',
    command: 'npm test',
    cwd: '/home/alice/work/web/app',
    machine_id: 'M2',
    workspace_id: 'W2',
    status: 'running',
    exit: null,
    signal: null,
    started_at: 1,
    ended_at: null,
    timeout_s: 600,
    log_path: '/home/alice/work/web/.controlroom/jobs/J1.log',
    lines: 12,
    follow: null,
  };
  const followed = { ...running, follow: { buffer_seconds: 15, pattern: 'FAIL', next_wake_at: null } };
  daemon.handlers.set('job.run', () => ({ result: { job_id: 'J1', log_path: running.log_path } }));
  daemon.handlers.set('job.follow', () => ({ result: followed }));
  daemon.handlers.set('job.status', () => ({ result: { jobs: [followed] } }));
  daemon.handlers.set('job.kill', () => ({ result: running }));
  const pi = await started(daemon, TWO);

  const run = await pi.run('job', { action: 'run', command: 'npm test', name: 'tests', timeout: 600, at: 'buildbox:app' }, { callId: 'tc-run' });
  assert.match(run.text, /Job J1 started on buildbox; its log is buildbox:~\/work\/web\/\.controlroom\/jobs\/J1\.log\. Its exit will wake you/);
  assert.deepEqual(daemon.paramsOf('job.run').at(-1), { call_id: 'tc-run', command: 'npm test', machine: 'buildbox', cwd: 'app', timeout_s: 600, name: 'tests' });
  const follow = await pi.run('job', { action: 'follow', job_id: 'J1', pattern: 'FAIL' }, { callId: 'tc-f' });
  assert.match(follow.text, /Following job J1: new lines matching \/FAIL\/ wake you after 15s/);
  assert.deepEqual(daemon.paramsOf('job.follow').at(-1), { call_id: 'tc-f', job_id: 'J1', pattern: 'FAIL' });
  const status = await pi.run('job', { action: 'status' });
  assert.equal(status.text, 'Job J1 `tests`: running, followed /FAIL/, 12 lines, log buildbox:~/work/web/.controlroom/jobs/J1.log');
  assert.match((await pi.run('job', { action: 'kill', job_id: 'J1' })).text, /Stopping job J1/);
  await assert.rejects(pi.run('job', { action: 'run' }), /needs a command/);
  await assert.rejects(pi.run('job', { action: 'run', command: 'make' }), /No directory given, and this session has several workspaces/);
  await assert.rejects(pi.run('job', { action: 'unfollow' }), /needs a job_id/);
  await pi.fire('session_shutdown', {});
});
