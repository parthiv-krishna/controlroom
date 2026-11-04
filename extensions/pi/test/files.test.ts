import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { FakeDaemon, hostOps, ONE, rpcError, TWO } from './fake-daemon.ts';
import { started } from './fake-pi.ts';

let daemon: FakeDaemon;
before(async () => {
  daemon = await FakeDaemon.start();
});
after(() => daemon.stop());

const MAIN = '/home/alice/src/api/src/main.rs';
const lines = (n: number, from = 1) => Array.from({ length: n }, (_, i) => `line ${from + i}`);
const textRead = (content: Record<string, unknown>) => () => ({
  path: MAIN,
  size: 100,
  content: { kind: 'text', start_line: 1, cut: null, first_line_too_long: false, ...content },
});

test('read sends machine and path, and shows the file like pi', async () => {
  daemon.handlers.set('host.call', hostOps({ read: textRead({ lines: ['fn main() {}', ''], total_lines: 2 }) }));
  const pi = await started(daemon, TWO);
  const out = await pi.run('read', { path: 'devbox:src/main.rs' }, { callId: 'tc-r' });
  assert.equal(out.text, 'fn main() {}\n');
  assert.equal(out.details, undefined);
  assert.deepEqual(daemon.paramsOf('host.call').at(-1), { call_id: 'tc-r', machine: 'devbox', op: { op: 'read', path: 'src/main.rs' } });
  await pi.fire('session_shutdown', {});
});

test('read windows carry pi notices and truncation details', async () => {
  const pi = await started(daemon, ONE);
  daemon.handlers.set('host.call', hostOps({ read: textRead({ lines: lines(2000), total_lines: 5000, cut: 'lines' }) }));
  const cut = await pi.run('read', { path: 'big.log' });
  assert.match(cut.text, /line 2000\n\n\[Showing lines 1-2000 of 5000\. Use offset=2001 to continue\.\]$/);
  assert.deepEqual((cut.details as { truncation: { truncatedBy: string } }).truncation.truncatedBy, 'lines');

  daemon.handlers.set('host.call', hostOps({ read: textRead({ lines: lines(10, 41), start_line: 41, total_lines: 5000, cut: 'bytes' }) }));
  assert.match((await pi.run('read', { path: 'big.log', offset: 41 })).text, /\[Showing lines 41-50 of 5000 \(50KB limit\)\. Use offset=51 to continue\.\]$/);

  daemon.handlers.set('host.call', hostOps({ read: textRead({ lines: lines(5, 11), start_line: 11, total_lines: 100 }) }));
  const limited = await pi.run('read', { path: 'devbox:big.log', offset: 11, limit: 5 });
  assert.match(limited.text, /line 15\n\n\[85 more lines in file\. Use offset=16 to continue\.\]$/);
  assert.deepEqual((daemon.paramsOf('host.call').at(-1)!.op as object), { op: 'read', path: 'big.log', offset: 11, limit: 5 });

  daemon.handlers.set('host.call', hostOps({ read: textRead({ lines: [], start_line: 7, total_lines: 9, first_line_too_long: true }) }));
  assert.match((await pi.run('read', { path: 'min.js', offset: 7 })).text, /^\[Line 7 of devbox:~\/src\/api\/src\/main\.rs exceeds the 50KB limit\. Use bash: sed -n '7p' \/home\/alice\/src\/api\/src\/main\.rs \| head -c 51200\]$/);

  daemon.handlers.set('host.call', hostOps({ read: textRead({ lines: [], start_line: 30, total_lines: 9 }) }));
  await assert.rejects(pi.run('read', { path: 'min.js', offset: 30 }), /Offset 30 is beyond end of file \(9 lines total\)/);
  await pi.fire('session_shutdown', {});
});

test('an image is fetched as bytes and attached by pi', async () => {
  const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';
  const size = Buffer.from(png, 'base64').length;
  daemon.handlers.set(
    'host.call',
    hostOps({
      read: () => ({ path: '/home/alice/src/api/logo.png', size, content: { kind: 'image', mime_type: 'image/png' } }),
      read_bytes: () => ({ path: '/home/alice/src/api/logo.png', data: png, length: size, size, sha256: 'x' }),
    }),
  );
  const pi = await started(daemon, ONE);
  const out = await pi.run('read', { path: 'logo.png' }, { callId: 'tc-img' });
  assert.ok(out.content.some((c) => c.type === 'image' && c.mimeType === 'image/png'), out.text);
  assert.match(out.text, /Read image file \[image\/png\]/);
  const calls = daemon.paramsOf('host.call').slice(-2);
  assert.deepEqual(calls[1], { call_id: 'tc-img/bytes', machine: 'devbox', op: { op: 'read_bytes', path: '/home/alice/src/api/logo.png' } });
  await pi.fire('session_shutdown', {});
});

test('read errors name the machine and the path', async () => {
  daemon.handlers.set('host.call', () => rpcError('fs_error', { path: 'src/nope.rs', detail: 'No such file or directory (os error 2)' }));
  const pi = await started(daemon, ONE);
  await assert.rejects(pi.run('read', { path: 'src/nope.rs' }), /^Error: devbox:src\/nope\.rs: No such file or directory/);
  await assert.rejects(pi.run('read', { path: 'devbox:' }), /read needs a file: machine:path/);
  await pi.fire('session_shutdown', {});
});

test('write reports the bytes written, the home shown as ~, and link drops', async () => {
  daemon.handlers.set(
    'host.call',
    hostOps({ write: (op) => ({ path: '/home/alice/work/web/notes.md', bytes: Buffer.byteLength(op.content as string), sha256: 'x' }) }, 2),
  );
  const pi = await started(daemon, TWO);
  const out = await pi.run('write', { path: 'buildbox:~/work/web/notes.md', content: 'héllo\n' }, { callId: 'tc-w' });
  assert.match(out.text, /^Successfully wrote 7 bytes to buildbox:~\/work\/web\/notes\.md\n\n\[The link to buildbox dropped 2 times during this call; the result came from the host's journal, so it ran once\.\]$/);
  assert.deepEqual(daemon.paramsOf('host.call').at(-1), {
    call_id: 'tc-w',
    machine: 'buildbox',
    op: { op: 'write', path: '~/work/web/notes.md', content: 'héllo\n' },
  });
  await assert.rejects(pi.run('write', { path: 'notes.md', content: 'x' }), /"notes\.md" names no machine/);
  await pi.fire('session_shutdown', {});
});

/** A host serving one file for edits: `read_bytes` and a compare-and-swap `write`. */
function editableFile(initial: string, onWrite?: (op: Record<string, unknown>) => void) {
  let content = initial;
  let sha = 'sha-0';
  return hostOps({
    read_bytes: () => ({ path: MAIN, data: Buffer.from(content).toString('base64'), length: Buffer.byteLength(content), size: Buffer.byteLength(content), sha256: sha }),
    write: (op) => {
      onWrite?.(op);
      if (op.expect_sha256 !== sha) return rpcError('conflict', { path: MAIN });
      content = op.content as string;
      sha = `sha-${Number(sha.slice(4)) + 1}`;
      return { path: MAIN, bytes: Buffer.byteLength(content), sha256: sha };
    },
  });
}

test("edit runs pi's edit over read_bytes and a compare-and-swap write", async () => {
  const writes: Record<string, unknown>[] = [];
  daemon.handlers.set('host.call', editableFile('fn main() {\n    println!("hi");\n}\n', (op) => writes.push(op)));
  const pi = await started(daemon, ONE);
  const out = await pi.run('edit', { path: 'src/main.rs', edits: [{ oldText: 'println!("hi");', newText: 'println!("hello");' }] }, { callId: 'tc-e' });
  assert.equal(out.text, 'Successfully replaced 1 block(s) in devbox:src/main.rs.');
  const details = out.details as { diff: string; patch: string };
  assert.match(details.diff, /\+.*hello/);
  assert.match(details.patch, /devbox:src\/main\.rs/);
  const steps = daemon.paramsOf('host.call').slice(-2);
  assert.deepEqual(steps[0], { call_id: 'tc-e/read', machine: 'devbox', op: { op: 'read_bytes', path: 'src/main.rs' } });
  assert.equal(steps[1].call_id, 'tc-e/write');
  assert.deepEqual(writes, [{ op: 'write', path: MAIN, content: 'fn main() {\n    println!("hello");\n}\n', expect_sha256: 'sha-0' }]);

  // pi's matching: a missing text is an error naming the file; nothing is written.
  await assert.rejects(pi.run('edit', { path: 'devbox:src/main.rs', edits: [{ oldText: 'nope', newText: 'x' }] }), /devbox:src\/main\.rs/);
  assert.equal(writes.length, 1);
  await pi.fire('session_shutdown', {});
});

test('an edit of a file changed meanwhile is a conflict, never a lost update', async () => {
  let other = false;
  // Another writer changes the file between this edit's read and its write.
  const file = editableFile('a = 1\n');
  daemon.handlers.set('host.call', async (params) => {
    const op = params.op as { op: string };
    if (op.op === 'write' && !other) {
      other = true;
      await file({ call_id: 'other', machine: 'devbox', op: { op: 'write', path: MAIN, content: 'a = 2\n', expect_sha256: 'sha-0' } });
    }
    return file(params);
  });
  const pi = await started(daemon, ONE);
  await assert.rejects(
    pi.run('edit', { path: 'devbox:src/main.rs', edits: [{ oldText: 'a = 1', newText: 'a = 3' }] }),
    /devbox:~\/src\/api\/src\/main\.rs changed after it was read \(another session or the user wrote it\), so nothing was written\. Read it again and redo the change\./,
  );
  await pi.fire('session_shutdown', {});
});

test('a file too large to read whole cannot be edited', async () => {
  daemon.handlers.set('host.call', hostOps({ read_bytes: () => ({ path: MAIN, data: '', length: 0, size: 30_000_000, sha256: null }) }));
  const pi = await started(daemon, ONE);
  await assert.rejects(pi.run('edit', { path: 'big.bin', edits: [{ oldText: 'a', newText: 'b' }] }), /too large to edit/);
  await pi.fire('session_shutdown', {});
});

test('aborting a read stops waiting without interrupting anything', async () => {
  daemon.handlers.set('host.call', () => new Promise(() => {}));
  const pi = await started(daemon, ONE);
  const abort = new AbortController();
  const reading = pi.run('read', { path: 'slow.txt' }, { callId: 'tc-slow', signal: abort.signal });
  await daemon.called((c) => c.method === 'host.call' && c.params.call_id === 'tc-slow');
  abort.abort();
  await assert.rejects(reading, /Operation aborted/);
  assert.equal(daemon.paramsOf('host.cancel').length, 0);
  await pi.fire('session_shutdown', {});
});
