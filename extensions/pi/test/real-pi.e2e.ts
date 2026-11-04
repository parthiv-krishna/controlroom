// Real pi with @controlroom/pi against a fake agent daemon and a scripted mock model (opt-in,
// like an #[ignore] test: it needs pi). pi runs as the agent daemon will run it
// (`--no-builtin-tools --no-context-files --no-skills`, only this extension) and the model
// drives the remote tools over two fake machines backed by local directories.
//
//   CONTROLROOM_TEST_PI=$(command -v pi) node --experimental-strip-types --test test/real-pi.e2e.ts

import assert from 'node:assert/strict';
import { type ChildProcess, spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { after, before, test } from 'node:test';

import { FakeDaemon } from './fake-daemon.ts';
import { FakeHost } from './real-pi/fake-host.ts';
import { MockModel, type ModelRequest } from './real-pi/mock-model.ts';

const PI = process.env.CONTROLROOM_TEST_PI;
const EXTENSION = join(import.meta.dirname, '..', 'src', 'index.ts');

let root: string;
let daemon: FakeDaemon;
let host: FakeHost;
let model: MockModel;
let pi: Pi;

/** pi in RPC mode: commands in, events out. */
class Pi {
  readonly events: Record<string, unknown>[] = [];
  #child: ChildProcess;
  #waiters: (() => void)[] = [];
  #nextId = 1;

  constructor(args: string[], env: Record<string, string>, cwd: string) {
    this.#child = spawn(PI!, args, { cwd, env, stdio: ['pipe', 'pipe', 'inherit'] });
    createInterface({ input: this.#child.stdout! }).on('line', (line) => {
      try {
        this.events.push(JSON.parse(line));
      } catch {
        return;
      }
      for (const wake of this.#waiters.splice(0)) wake();
    });
  }

  send(command: Record<string, unknown>): void {
    this.#child.stdin!.write(JSON.stringify({ id: `c${this.#nextId++}`, ...command }) + '\n');
  }

  /** Resolves with the first event (after `from`) matching `match`. */
  async next(match: (e: Record<string, unknown>) => boolean, from = 0): Promise<Record<string, unknown>> {
    for (;;) {
      const found = this.events.slice(from).find(match);
      if (found) return found;
      await new Promise<void>((resolve) => this.#waiters.push(resolve));
    }
  }

  stop(): void {
    this.#child.kill('SIGKILL');
  }
}

/** A tool result pi sent the model, as text. */
function toolResult(request: ModelRequest, id: string): string {
  const message = request.messages.find((m) => m.role === 'tool' && m.tool_call_id === id);
  assert.ok(message, `no result for ${id}`);
  const content = message.content;
  return typeof content === 'string' ? content : JSON.stringify(content);
}

before(async () => {
  if (!PI) return;
  root = mkdtempSync(join(tmpdir(), 'cr-real-pi-'));
  const machine = (name: string, dir: string) => {
    const home = join(root, name, 'home', 'alice');
    mkdirSync(join(home, dir), { recursive: true });
    return { name, home, dir };
  };
  const devbox = machine('devbox', 'src/api');
  const buildbox = machine('buildbox', 'work/web');
  writeFileSync(join(devbox.home, devbox.dir, 'AGENTS.md'), 'Run cargo test before committing.\n');
  mkdirSync(join(devbox.home, devbox.dir, 'src'));
  writeFileSync(join(devbox.home, devbox.dir, 'src', 'main.rs'), 'fn main() {\n    println!("hello");\n}\n');

  daemon = await FakeDaemon.start();
  host = new FakeHost(daemon, [devbox, buildbox]);
  model = await MockModel.start([
    {
      tools: [
        { id: 'r1', name: 'read', args: { path: 'devbox:src/main.rs' } },
        { id: 'r2', name: 'read', args: { path: 'README.md' } },
      ],
    },
    { tools: [{ id: 'e1', name: 'edit', args: { path: 'devbox:src/main.rs', edits: [{ oldText: '"hello"', newText: '"hello, world"' }] } }] },
    {
      tools: [
        { id: 'b1', name: 'bash', args: { command: 'cd src && echo $((6*7))', shell: 'devbox' } },
        { id: 'w1', name: 'write', args: { path: 'buildbox:notes.md', content: 'built\n' } },
      ],
    },
    {
      tools: [
        { id: 'g1', name: 'grep', args: { pattern: 'hello', path: 'devbox:' } },
        { id: 'l1', name: 'ls', args: { path: 'buildbox:' } },
      ],
    },
    { text: 'Done.' },
  ]);

  const agent = join(root, 'agent');
  mkdirSync(agent);
  const cost = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
  const models = { id: 'mock-model', name: 'Mock', input: ['text'], contextWindow: 128000, maxTokens: 4096, cost };
  writeFileSync(
    join(agent, 'models.json'),
    JSON.stringify({ providers: { mock: { baseUrl: model.baseUrl, api: 'openai-completions', apiKey: 'sk-fake-test-key', models: [models] } } }),
  );
  writeFileSync(join(agent, 'settings.json'), JSON.stringify({ defaultProvider: 'mock', defaultModel: 'mock-model', quietStartup: true }));
  const scratch = join(root, 'scratch');
  mkdirSync(join(root, 'pihome'));
  mkdirSync(scratch);
  const env = {
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    HOME: join(root, 'pihome'),
    PI_CODING_AGENT_DIR: agent,
    PI_OFFLINE: '1',
    PI_TELEMETRY: '0',
    PI_SKIP_VERSION_CHECK: '1',
    CONTROLROOM_SESSION_ID: 'S',
    CONTROLROOM_AGENT_SOCKET: daemon.socket,
    CONTROLROOM_AGENT_TOKEN: daemon.token,
  };
  const args = ['--mode', 'rpc', '--no-session', '--no-themes', '--no-extensions', '-e', EXTENSION, '--no-builtin-tools', '--no-context-files', '--no-skills'];
  pi = new Pi(args, env, scratch);
});

after(async () => {
  if (!PI) return;
  pi?.stop();
  host?.stop();
  await model?.stop();
  await daemon?.stop();
  rmSync(root, { recursive: true, force: true });
});

test('real pi drives the remote tools on two machines', { skip: !PI && 'set CONTROLROOM_TEST_PI to a pi 1.2.0 binary' }, async () => {
  pi.send({ type: 'prompt', message: 'Change the greeting and record the build.' });
  await pi.next((e) => e.type === 'agent_settled');
  await model.requestCount(5);
  const [first, afterReads, afterEdit, afterBash, afterSearch] = model.requests;

  // The model sees exactly the extension's tools: pi's built-ins are replaced, not duplicated.
  const tools = (first.tools ?? []).map((t) => t.function.name).sort();
  assert.deepEqual(tools, ['ask_user', 'bash', 'edit', 'find', 'grep', 'job', 'ls', 'notify_user', 'read', 'shell_list', 'shell_new', 'todo', 'write']);

  // The system prompt: the workspaces, their instruction files as machine:path, no local cwd.
  const system = JSON.stringify(first.messages.filter((m) => m.role === 'system' || m.role === 'developer'));
  assert.match(system, /<workspaces>/);
  assert.match(system, /- devbox:~\/src\/api \(primary; root devbox:~\/src\/api; default shell \\"devbox\\"\)/);
  assert.match(system, /project_instructions path=\\"devbox:~\/src\/api\/AGENTS\.md\\"/);
  assert.match(system, /Run cargo test before committing\./);
  assert.match(system, /none on this machine: pi runs on the controlroom server/);
  assert.doesNotMatch(system, /scratch/);

  assert.match(toolResult(afterReads, 'r1'), /println!\("hello"\)/);
  assert.match(toolResult(afterReads, 'r2'), /"README\.md" names no machine, and this session has several workspaces/);
  assert.match(toolResult(afterEdit, 'e1'), /Successfully replaced 1 block\(s\) in devbox:src\/main\.rs\./);
  const bash = toolResult(afterBash, 'b1');
  assert.match(bash, /\[shell devbox · devbox:~\/src\/api\/src · exit 0\]/);
  assert.match(bash, /42/);
  assert.match(toolResult(afterBash, 'w1'), /Successfully wrote 6 bytes to buildbox:~\/work\/web\/notes\.md/);
  assert.match(toolResult(afterSearch, 'g1'), /\[in devbox:~\/src\/api\]\nsrc\/main\.rs:2:/);
  assert.match(toolResult(afterSearch, 'l1'), /notes\.md/);

  // The effects happened on the "hosts", once.
  const devboxHome = join(root, 'devbox', 'home', 'alice');
  assert.equal(readFileSync(join(devboxHome, 'src/api/src/main.rs'), 'utf8'), 'fn main() {\n    println!("hello, world");\n}\n');
  assert.equal(readFileSync(join(root, 'buildbox', 'home', 'alice', 'work/web/notes.md'), 'utf8'), 'built\n');
  assert.deepEqual(host.ran, ['b1']);
  // The edit read and wrote with step ids; the write was a compare-and-swap.
  const steps = daemon.paramsOf('host.call').filter((p) => String(p.call_id).startsWith('e1/'));
  assert.deepEqual(steps.map((p) => p.call_id), ['e1/read', 'e1/write']);
  assert.match(String((steps[1].op as { expect_sha256: string }).expect_sha256), /^[0-9a-f]{64}$/);
});

test('aborting a run interrupts the remote command', { skip: !PI && 'set CONTROLROOM_TEST_PI to a pi 1.2.0 binary' }, async () => {
  model.script({ tools: [{ id: 'b2', name: 'bash', args: { command: 'echo started; sleep 30', shell: 'buildbox' } }] });
  const from = pi.events.length;
  pi.send({ type: 'prompt', message: 'Run the long build.' });
  await daemon.called((c) => c.method === 'host.call' && c.params.call_id === 'b2');
  await pi.next((e) => e.type === 'tool_execution_update' && JSON.stringify(e.partialResult ?? null).includes('started'), from);
  pi.send({ type: 'abort' });
  const end = await pi.next((e) => e.type === 'tool_execution_end' && e.toolCallId === 'b2', from);
  await pi.next((e) => e.type === 'agent_settled', from);
  assert.deepEqual(daemon.paramsOf('host.cancel'), [{ call_id: 'b2' }]);
  const text = JSON.stringify(end.result);
  assert.match(text, /\[shell buildbox · buildbox:~\/work\/web · exit 130\]/);
  assert.match(text, /started/);
  assert.match(text, /\[Command aborted\]/);
});
