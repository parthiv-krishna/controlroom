import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { parseTarget, shown } from '../src/remote/target.ts';
import { FakeDaemon, hostOps, ONE, TWO, workspace } from './fake-daemon.ts';
import { started } from './fake-pi.ts';

let daemon: FakeDaemon;
before(async () => {
  daemon = await FakeDaemon.start();
});
after(() => daemon.stop());

test('targets: machine:path, bare paths with one workspace, refusals with a hint', () => {
  assert.deepEqual(parseTarget(TWO, 'buildbox:~/work/web/a.ts'), { machine: 'buildbox', path: '~/work/web/a.ts', display: 'buildbox:~/work/web/a.ts' });
  assert.deepEqual(parseTarget(TWO, 'devbox:/etc/hosts'), { machine: 'devbox', path: '/etc/hosts', display: 'devbox:/etc/hosts' });
  assert.deepEqual(parseTarget(TWO, 'devbox:'), { machine: 'devbox', path: null, display: 'devbox:~/src/api' });
  assert.deepEqual(parseTarget(ONE, 'src/main.rs'), { machine: 'devbox', path: 'src/main.rs', display: 'devbox:src/main.rs' });
  assert.deepEqual(parseTarget(ONE, undefined), { machine: 'devbox', path: null, display: 'devbox:~/src/api' });
  assert.throws(
    () => parseTarget(TWO, 'src/main.rs'),
    /^Error: "src\/main\.rs" names no machine, and this session has several workspaces: write machine:path, e\.g\. devbox:src\/main\.rs\. Workspaces: devbox:~\/src\/api, buildbox:~\/work\/web\.$/,
  );
  assert.throws(() => parseTarget(TWO, ''), /No path given, and this session has several workspaces: write machine:path, e\.g\. devbox:~\/src\/api\./);
  assert.throws(() => parseTarget(ONE, 'webbox:index.html'), /Unknown machine "webbox" in path "webbox:index\.html"\. Workspaces: devbox:~\/src\/api\./);
  assert.throws(() => parseTarget({ workspaces: [], shells: [] }, 'x'), /no workspaces yet/);
  // Several workspaces on one machine still need the machine named.
  const same = { workspaces: [workspace('devbox', 'a'), workspace('devbox', 'b')], shells: [] };
  assert.throws(() => parseTarget(same, 'x'), /names no machine/);
  assert.equal(shown(ONE, 'devbox', '/home/alice'), 'devbox:~');
  assert.equal(shown(ONE, 'devbox', '/home/alicex/a'), 'devbox:/home/alicex/a');
});

test('grep shows where it searched and pi-style matches, context and notices', async () => {
  daemon.handlers.set(
    'host.call',
    hostOps({
      grep: () => ({
        path: '/home/alice/src/api',
        lines: [
          { path: 'src/a.rs', line: 3, text: 'fn todo() {}', context: false },
          { path: 'src/a.rs', line: 4, text: '}', context: true },
        ],
        matches: 1,
        limit_reached: true,
        lines_truncated: true,
      }),
    }),
  );
  const pi = await started(daemon, ONE);
  const out = await pi.run('grep', { pattern: 'todo', glob: '*.rs', ignoreCase: true, context: 1, limit: 1 }, { callId: 'tc-g' });
  assert.equal(
    out.text,
    '[in devbox:~/src/api]\nsrc/a.rs:3: fn todo() {}\nsrc/a.rs-4- }\n\n' +
      '[1 matches limit reached. Use limit=2 for more, or refine pattern. Some lines truncated to 500 chars. Use read to see full lines]',
  );
  assert.deepEqual(out.details, { matchLimitReached: 1, linesTruncated: true });
  assert.deepEqual(daemon.paramsOf('host.call').at(-1), {
    call_id: 'tc-g',
    machine: 'devbox',
    op: { op: 'grep', pattern: 'todo', path: null, glob: '*.rs', ignore_case: true, literal: false, context: 1, limit: 1 },
  });

  daemon.handlers.set('host.call', hostOps({ grep: () => ({ path: '/srv/www', lines: [], matches: 0, limit_reached: false, lines_truncated: false }) }));
  assert.equal((await pi.run('grep', { pattern: 'x', path: 'devbox:/srv/www' })).text, '[in devbox:/srv/www]\nNo matches found');
  await pi.fire('session_shutdown', {});
});

test('grep output over 50KB is cut at a line with a notice', async () => {
  const long = Array.from({ length: 100 }, (_, i) => ({ path: 'big.txt', line: i + 1, text: 'x'.repeat(499), context: false }));
  daemon.handlers.set('host.call', hostOps({ grep: () => ({ path: '/home/alice/src/api', lines: long, matches: 100, limit_reached: false, lines_truncated: false }) }));
  const pi = await started(daemon, ONE);
  const out = await pi.run('grep', { pattern: 'x' });
  assert.ok(Buffer.byteLength(out.text) < 51 * 1024);
  assert.match(out.text, /\n\n\[50KB limit reached\]$/);
  assert.equal((out.details as { truncation: { truncated: boolean } }).truncation.truncated, true);
  await pi.fire('session_shutdown', {});
});

test('find lists relative paths under where it searched', async () => {
  daemon.handlers.set('host.call', hostOps({ find: (op) => ({ path: '/home/alice/work/web', paths: ['a.test.ts', 'src/b.test.ts'], limit_reached: op.limit === 2 }) }));
  const pi = await started(daemon, TWO);
  const out = await pi.run('find', { pattern: '*.test.ts', path: 'buildbox:', limit: 2 });
  assert.equal(out.text, '[in buildbox:~/work/web]\na.test.ts\nsrc/b.test.ts\n\n[2 results limit reached. Use limit=4 for more, or refine pattern]');
  assert.deepEqual(daemon.paramsOf('host.call').at(-1)!.op, { op: 'find', pattern: '*.test.ts', path: null, limit: 2 });
  await assert.rejects(pi.run('find', { pattern: '*.rs' }), /No path given, and this session has several workspaces/);
  daemon.handlers.set('host.call', hostOps({ find: () => ({ path: '/home/alice/work/web', paths: [], limit_reached: false }) }));
  assert.equal((await pi.run('find', { pattern: '*.zig', path: 'buildbox:' })).text, '[in buildbox:~/work/web]\nNo files found matching pattern');
  await pi.fire('session_shutdown', {});
});

test('ls lists entries with / for directories and a limit notice', async () => {
  const entry = (name: string, kind: string) => ({ name, kind, size: null, mtime: null });
  daemon.handlers.set('host.call', hostOps({ ls: () => ({ path: '/home/alice/src/api', entries: [entry('.git', 'dir'), entry('Cargo.toml', 'file'), entry('src', 'dir')], total: 7 }) }));
  const pi = await started(daemon, ONE);
  const out = await pi.run('ls', { limit: 3 });
  assert.equal(out.text, '.git/\nCargo.toml\nsrc/\n\n[3 entries limit reached (7 in total). Use limit=6 for more]');
  assert.deepEqual(out.details, { entryLimitReached: 3 });
  assert.deepEqual(daemon.paramsOf('host.call').at(-1)!.op, { op: 'ls', path: null, limit: 3 });
  daemon.handlers.set('host.call', hostOps({ ls: () => ({ path: '/home/alice/src/api/empty', entries: [], total: 0 }) }));
  assert.equal((await pi.run('ls', { path: 'empty' })).text, '(empty directory)');
  await pi.fire('session_shutdown', {});
});

test('a result of the wrong op is refused', async () => {
  daemon.handlers.set('host.call', () => ({ result: { result: { op: 'unknown' }, link_drops: 0 } }));
  const pi = await started(daemon, ONE);
  await assert.rejects(pi.run('ls', {}), /devbox answered a ls call with a unknown result/);
  await pi.fire('session_shutdown', {});
});
