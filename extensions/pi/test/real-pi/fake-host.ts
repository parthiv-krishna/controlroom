// The agent daemon side of the real-pi test: `session.info`, `shell.*`, `host.call` and
// `host.cancel` served from local directories standing in for the machines (one directory tree
// per machine, with alice's home inside it). Just enough of each op for the scenario; the
// real host daemon is R1a's.

import { spawn, type ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative } from 'node:path';

import type { HostOp } from '../../src/protocol/HostOp.ts';
import type { SessionInfo } from '../../src/protocol/SessionInfo.ts';
import { type FakeDaemon, type Reply, rpcError, shell, workspace } from '../fake-daemon.ts';

/** One machine: its home directory on the local disk, and its workspace under it. */
export interface Machine {
  name: string;
  home: string;
  /** The workspace, relative to the home. */
  dir: string;
}

export class FakeHost {
  readonly info: SessionInfo;
  /** Commands started (by call id), to count runs. */
  readonly ran: string[] = [];
  #machines: Machine[];
  #cwd = new Map<string, string>();
  #running = new Map<string, ChildProcess>();

  constructor(daemon: FakeDaemon, machines: Machine[]) {
    this.#machines = machines;
    this.info = {
      workspaces: machines.map((m, i) =>
        workspace(m.name, m.dir, { primary: i === 0, home: m.home, root: join(m.home, m.dir), context_files: this.#contextFiles(m) }),
      ),
      shells: machines.map((m) => shell(m.name, m.name, join(m.home, m.dir))),
    };
    for (const s of this.info.shells) this.#cwd.set(s.name, s.cwd);
    daemon.handlers.set('session.info', () => ({ result: this.info }));
    daemon.handlers.set('shell.list', () => ({ result: { shells: this.info.shells } }));
    daemon.handlers.set('host.cancel', (p) => {
      // Ctrl-C reaches the command's whole process group, as in a terminal.
      const pid = this.#running.get(p.call_id as string)?.pid;
      if (pid) process.kill(-pid, 'SIGINT');
      return { result: {} };
    });
    daemon.handlers.set('host.call', (p) => this.#call(daemon, p.call_id as string, p.machine as string, p.op as HostOp));
  }

  #machine(name: string): Machine {
    const m = this.#machines.find((x) => x.name === name);
    if (!m) throw new Error(`no machine ${name}`);
    return m;
  }

  #resolve(m: Machine, path: string | null | undefined): string {
    if (!path) return join(m.home, m.dir);
    if (path === '~' || path.startsWith('~/')) return join(m.home, path.slice(1));
    return isAbsolute(path) ? path : join(m.home, m.dir, path);
  }

  #contextFiles(m: Machine) {
    const file = join(m.home, m.dir, 'AGENTS.md');
    try {
      return [{ path: file, content: readFileSync(file, 'utf8') }];
    } catch {
      return [];
    }
  }

  async #call(daemon: FakeDaemon, callId: string, machine: string, op: HostOp): Promise<Reply> {
    const m = this.#machine(machine);
    const ok = (result: object): Reply => ({ result: { result: { op: op.op, ...result }, link_drops: 0 } });
    try {
      switch (op.op) {
        case 'read': {
          const path = this.#resolve(m, op.path);
          const all = readFileSync(path, 'utf8').split('\n');
          const start = op.offset ?? 1;
          const lines = all.slice(start - 1, op.limit ? start - 1 + op.limit : undefined);
          return ok({ path, size: statSync(path).size, content: { kind: 'text', lines, start_line: start, total_lines: all.length, cut: null, first_line_too_long: false } });
        }
        case 'read_bytes': {
          const path = this.#resolve(m, op.path);
          const bytes = readFileSync(path);
          return ok({ path, data: bytes.toString('base64'), length: bytes.length, size: bytes.length, sha256: sha256(bytes) });
        }
        case 'write': {
          const path = this.#resolve(m, op.path);
          if (op.expect_sha256 && sha256(readFileSync(path)) !== op.expect_sha256) return rpcError('conflict', { path });
          mkdirSync(dirname(path), { recursive: true });
          writeFileSync(path, op.content);
          return ok({ path, bytes: Buffer.byteLength(op.content), sha256: sha256(Buffer.from(op.content)) });
        }
        case 'ls': {
          const path = this.#resolve(m, op.path);
          const names = readdirSync(path).sort();
          const entries = names.map((name) => ({ name, kind: statSync(join(path, name)).isDirectory() ? 'dir' : 'file', size: null, mtime: null }));
          return ok({ path, entries, total: entries.length });
        }
        case 'grep': {
          const path = this.#resolve(m, op.path);
          const pattern = new RegExp(op.pattern, op.ignore_case ? 'i' : '');
          const lines = walk(path).flatMap((file) =>
            readFileSync(file, 'utf8')
              .split('\n')
              .flatMap((text, i) => (pattern.test(text) ? [{ path: relative(path, file), line: i + 1, text, context: false }] : [])),
          );
          return ok({ path, lines, matches: lines.length, limit_reached: false, lines_truncated: false });
        }
        case 'bash':
          return ok(await this.#bash(daemon, callId, op.shell, op.command));
        default:
          return rpcError('unsupported', {}, `the fake host has no ${op.op}`);
      }
    } catch (error) {
      return rpcError('fs_error', { path: 'path' in op ? (op.path ?? '') : '', detail: (error as Error).message });
    }
  }

  /** Runs a command in the shell's directory and keeps the directory it ends in. */
  #bash(daemon: FakeDaemon, callId: string, name: string, command: string): Promise<object> {
    this.ran.push(callId);
    const cwd = this.#cwd.get(name) ?? '/';
    const script = `${command}\n__status=$?\nprintf '%s' "$PWD" >&3\nexit $__status`;
    const child = spawn('bash', ['-c', script], { cwd, detached: true, stdio: ['ignore', 'pipe', 'pipe', 'pipe'] });
    this.#running.set(callId, child);
    let output = '';
    let pwd = '';
    const onData = (chunk: Buffer) => {
      output += chunk.toString();
      daemon.notify('host.output', { call_id: callId, text: chunk.toString() });
    };
    child.stdout!.on('data', onData);
    child.stderr!.on('data', onData);
    (child.stdio[3] as NodeJS.ReadableStream).on('data', (chunk: Buffer) => (pwd += chunk.toString()));
    return new Promise((resolve) => {
      child.on('close', (code, signal) => {
        this.#running.delete(callId);
        if (pwd) this.#cwd.set(name, pwd);
        resolve({
          exit_code: signal ? 130 : code,
          signal: null,
          cwd: this.#cwd.get(name),
          output,
          truncated: false,
          log_path: join(this.#machine(this.info.shells.find((s) => s.name === name)!.machine).home, '.cr', `${callId}.log`),
          timed_out: false,
          cancelled: signal !== null,
          waited_for_input: false,
          shell_reset: null,
          duration_ms: 1,
        });
      });
    });
  }

  /** Kills commands still running (the test's cleanup). */
  stop(): void {
    for (const child of this.#running.values()) if (child.pid) process.kill(-child.pid, 'SIGKILL');
  }
}

function sha256(bytes: Buffer): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function walk(path: string): string[] {
  if (!statSync(path).isDirectory()) return [path];
  return readdirSync(path).flatMap((name) => walk(join(path, name)));
}
