import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import type { ToolResult } from '../src/tool.ts';
import { FakeDaemon, hostOps, ONE, type Reply, rpcError, shell, TWO } from './fake-daemon.ts';
import { started } from './fake-pi.ts';

let daemon: FakeDaemon;
before(async () => {
  daemon = await FakeDaemon.start();
});
after(() => daemon.stop());

/** A bash result with defaults. */
const bashResult = (over: Record<string, unknown> = {}) => ({
  exit_code: 0,
  signal: null,
  cwd: '/home/alice/src/api/src',
  output: 'hello\n',
  truncated: false,
  log_path: '/home/alice/.local/state/controlroom/calls/c.log',
  timed_out: false,
  cancelled: false,
  waited_for_input: false,
  shell_reset: null,
  duration_ms: 12,
  ...over,
});

/** The text of a partial update. */
const updateText = (u: ToolResult<unknown>) => u.content.map((c) => (c.type === 'text' ? c.text : '')).join('');

test('bash runs in the default shell and starts with [shell · cwd · exit]', async () => {
  daemon.handlers.set('host.call', hostOps({ bash: () => bashResult() }));
  const pi = await started(daemon, ONE);
  const out = await pi.run('bash', { command: 'cd src && echo hello', timeout: 30 }, { callId: 'tc-b' });
  assert.equal(out.text, '[shell devbox · devbox:~/src/api/src · exit 0]\nhello');
  assert.equal(out.isError, undefined);
  assert.deepEqual(daemon.paramsOf('host.call').at(-1), {
    call_id: 'tc-b',
    machine: 'devbox',
    op: { op: 'bash', shell: 'devbox', command: 'cd src && echo hello', timeout_s: 30 },
  });
  await pi.fire('session_shutdown', {});
});

test('bash streams output and the waiting-for-input state into tool updates', async () => {
  let finish: (reply: Reply) => void = () => {};
  daemon.handlers.set('host.call', () => new Promise((resolve) => (finish = resolve)));
  const pi = await started(daemon, TWO);
  const updates: string[] = [];
  const running = pi.run('bash', { command: 'make', shell: 'buildbox' }, { callId: 'tc-s', onUpdate: (u) => updates.push(updateText(u)) });
  await daemon.called((c) => c.method === 'host.call' && c.params.call_id === 'tc-s');
  daemon.notify('host.output', { call_id: 'other', text: 'not mine\n' });
  daemon.notify('host.output', { call_id: 'tc-s', text: 'step 1\n' });
  daemon.notify('host.waiting_input', { call_id: 'tc-s', waiting: true });
  daemon.notify('host.waiting_input', { call_id: 'tc-s', waiting: false });
  daemon.notify('host.output', { call_id: 'tc-s', text: 'step 2\n' });
  while (updates.length < 5) await new Promise((r) => setImmediate(r));
  finish({
    result: {
      result: { op: 'bash', ...bashResult({ exit_code: 2, cwd: '/tmp', output: 'step 1\nstep 2\n', waited_for_input: true }) },
      link_drops: 1,
    },
  });
  const out = await running;
  assert.deepEqual(updates, [
    '',
    'step 1\n',
    'step 1\n\n[The command is waiting for terminal input. The user can type it in the session terminal; you cannot.]',
    'step 1\n',
    'step 1\nstep 2\n',
  ]);
  assert.equal(
    out.text,
    '[shell buildbox · buildbox:/tmp · exit 2]\nstep 1\nstep 2\n[The command waited for terminal input.]\n' +
      "[The link to buildbox dropped once during this call; the result came from the host's journal, so it ran once.]",
  );
  assert.equal(out.isError, true);
  assert.equal(daemon.paramsOf('host.call').at(-1)!.machine, 'buildbox');
  await pi.fire('session_shutdown', {});
});

test('bash notes a cut output with its log, a timeout and a reopened shell', async () => {
  daemon.handlers.set(
    'host.call',
    hostOps({ bash: () => bashResult({ exit_code: null, signal: 'SIGINT', truncated: true, timed_out: true, shell_reset: 'the host daemon restarted' }) }),
  );
  const pi = await started(daemon, ONE);
  const out = await pi.run('bash', { command: 'sleep 999', timeout: 5 });
  assert.equal(
    out.text,
    [
      '[shell devbox · devbox:~/src/api/src · exit SIGINT]',
      'hello',
      '[Shell devbox was reopened before this command (the host daemon restarted): its variables, functions and venv were lost.]',
      '[Output cut to the last 2000 lines or 50KB. Full output: devbox:~/.local/state/controlroom/calls/c.log]',
      '[Command timed out after 5 seconds and was interrupted.]',
    ].join('\n'),
  );
  assert.deepEqual(out.details, { fullOutputPath: 'devbox:~/.local/state/controlroom/calls/c.log' });
  assert.equal(out.isError, true);
  await pi.fire('session_shutdown', {});
});

test('aborting bash interrupts the command on the host and returns its partial output', async () => {
  let finish: (reply: Reply) => void = () => {};
  daemon.handlers.set('host.call', () => new Promise((resolve) => (finish = resolve)));
  daemon.handlers.set('host.cancel', () => {
    finish({ result: { result: { op: 'bash', ...bashResult({ exit_code: 130, output: 'tick 1\n', cancelled: true }) }, link_drops: 0 } });
    return { result: {} };
  });
  const pi = await started(daemon, ONE);
  const abort = new AbortController();
  const running = pi.run('bash', { command: 'while true; do sleep 1; done' }, { callId: 'tc-loop', signal: abort.signal });
  await daemon.called((c) => c.method === 'host.call' && c.params.call_id === 'tc-loop');
  abort.abort();
  const out = await running;
  assert.deepEqual(daemon.paramsOf('host.cancel'), [{ call_id: 'tc-loop' }]);
  assert.equal(out.text, '[shell devbox · devbox:~/src/api/src · exit 130]\ntick 1\n[Command aborted]');
  assert.equal(out.isError, true);
  await pi.fire('session_shutdown', {});
});

test('bash failures the model can act on', async () => {
  const pi = await started(daemon, TWO);
  daemon.handlers.set('host.call', () => rpcError('connection_lost', { machine_id: 'M1', since: Date.UTC(2026, 0, 2, 3, 4, 5) }));
  await assert.rejects(
    pi.run('bash', { command: 'make' }, { callId: 'tc-cl' }),
    /^Error: devbox is unreachable since 2026-01-02T03:04:05\.000Z; the call may still be running there \(call tc-cl\)\. Its result will be delivered to you when devbox is back\.$/,
  );
  daemon.handlers.set('host.call', () => rpcError('lost', { may_have_run: true }));
  await assert.rejects(pi.run('bash', { command: 'make' }), /restarted while this call ran.*may have partly run/);
  daemon.handlers.set('host.call', () => rpcError('lost', { may_have_run: false }));
  await assert.rejects(pi.run('bash', { command: 'make' }), /nothing changed, so retrying is safe/);
  daemon.handlers.set('host.call', () => rpcError('shell_gone', { shell: 'devbox' }));
  await assert.rejects(pi.run('bash', { command: 'make' }), /Shell "devbox" is gone\. Open it again with shell_new\./);
  daemon.handlers.set('host.call', () => rpcError('not_found', { resource: 'workspace', id: 'devbox' }));
  await assert.rejects(pi.run('bash', { command: 'make' }), /"devbox" is not the machine of one of this session's workspaces\. Workspaces: devbox:~\/src\/api, buildbox:~\/work\/web\./);
  // An unknown shell is looked up again (it may be new), then refused with the list.
  daemon.handlers.set('shell.list', () => ({ result: { shells: TWO.shells } }));
  await assert.rejects(pi.run('bash', { command: 'make', shell: 'nope' }), /There is no shell "nope"\. Shells: devbox, buildbox\. Open one with shell_new\./);
  await pi.fire('session_shutdown', {});
});

test('a shell opened after the last session info is found through shell.list', async () => {
  daemon.handlers.set('shell.list', () => ({ result: { shells: [...TWO.shells, shell('tests', 'buildbox', '/home/alice/work/web')] } }));
  daemon.handlers.set('host.call', hostOps({ bash: () => bashResult({ cwd: '/home/alice/work/web' }) }));
  const pi = await started(daemon, TWO);
  const out = await pi.run('bash', { command: 'npm test', shell: 'tests' });
  assert.match(out.text, /^\[shell tests · buildbox:~\/work\/web · exit 0\]/);
  assert.equal(daemon.paramsOf('host.call').at(-1)!.machine, 'buildbox');
  await pi.fire('session_shutdown', {});
});

test('shell_new opens a named shell on a machine; shell_list lists them', async () => {
  daemon.handlers.set('shell.open', (p) => ({ result: shell(p.name as string, p.machine as string, '/home/alice/src/api/tests') }));
  const pi = await started(daemon, TWO);
  const out = await pi.run('shell_new', { name: 'devbox-tests', at: 'devbox:tests' }, { callId: 'tc-n' });
  assert.equal(out.text, 'Shell devbox-tests is open on devbox in devbox:~/src/api/tests. Use bash {shell: "devbox-tests"}.');
  assert.deepEqual(daemon.paramsOf('shell.open').at(-1), { call_id: 'tc-n', name: 'devbox-tests', machine: 'devbox', cwd: 'tests' });
  await assert.rejects(pi.run('shell_new', { name: 'x' }), /No directory given, and this session has several workspaces/);
  await assert.rejects(pi.run('shell_new', { name: ' ', at: 'devbox:' }), /needs a name/);

  // The name is taken on another machine.
  daemon.handlers.set('shell.open', (p) => ({ result: shell(p.name as string, 'buildbox', '/tmp') }));
  await assert.rejects(pi.run('shell_new', { name: 'buildbox', at: 'devbox:' }), /A shell named "buildbox" already exists on buildbox \(in buildbox:\/tmp\); choose another name\./);

  const gone = { ...shell('old', 'devbox', '/home/alice'), state: { state: 'gone' as const }, reset_pending: true };
  daemon.handlers.set('shell.list', () => ({ result: { shells: [...TWO.shells, gone] } }));
  assert.equal(
    (await pi.run('shell_list', {})).text,
    ['- devbox: devbox:~/src/api (idle)', '- buildbox: buildbox:/tmp (idle)', '- old: devbox:~ (gone; reopened, its variables are lost)'].join('\n'),
  );
  await pi.fire('session_shutdown', {});
});
