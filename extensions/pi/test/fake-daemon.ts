// A scriptable stand-in for the agent daemon's socket: hello with a token, then handlers per
// method (sync or async). Tests can drop connections and push notifications.

import { mkdtempSync, rmSync } from 'node:fs';
import { createServer, type Server, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import type { AgentNotificationName, AgentNotifications } from '../src/protocol/AgentMethods.ts';
import type { AgentShell } from '../src/protocol/AgentShell.ts';
import type { SessionInfo } from '../src/protocol/SessionInfo.ts';
import type { Todo } from '../src/protocol/Todo.ts';
import type { TodoStatus } from '../src/protocol/TodoStatus.ts';
import type { WorkspaceInfo } from '../src/protocol/WorkspaceInfo.ts';

/** A handler's reply: a result, or an error object `{code, message, data}`. */
export type Reply = { result?: unknown; error?: unknown };

/** A method handler. */
export type Handler = (params: Record<string, unknown>) => Reply | Promise<Reply>;

export class FakeDaemon {
  readonly socket: string;
  readonly token = 'a'.repeat(64);
  readonly calls: { method: string; params: Record<string, unknown> }[] = [];
  handlers = new Map<string, Handler>();
  /** Answer nothing for these methods (to test drops mid-call). */
  silent = new Set<string>();
  #dir: string;
  #server: Server;
  #conns = new Set<Socket>();
  #hellos = 0;
  #waiters: (() => void)[] = [];

  private constructor(dir: string) {
    this.#dir = dir;
    this.socket = join(dir, 'agent.sock');
    this.#server = createServer((conn) => this.#serve(conn));
  }

  static async start(): Promise<FakeDaemon> {
    const daemon = new FakeDaemon(mkdtempSync(join(tmpdir(), 'cr-pi-')));
    await new Promise<void>((resolve) => daemon.#server.listen(daemon.socket, resolve));
    return daemon;
  }

  /** Successful hellos so far. */
  get hellos(): number {
    return this.#hellos;
  }

  /** Resolves once `n` hellos succeeded (a gate, not a sleep). */
  async helloCount(n: number): Promise<void> {
    while (this.#hellos < n) await new Promise<void>((resolve) => this.#waiters.push(resolve));
  }

  /** Resolves once a call matching `match` arrived; returns it. */
  async called(match: (c: { method: string; params: Record<string, unknown> }) => boolean): Promise<Record<string, unknown>> {
    for (;;) {
      const found = this.calls.find(match);
      if (found) return found.params;
      await new Promise((resolve) => setImmediate(resolve));
    }
  }

  /** The params of every call of `method`. */
  paramsOf(method: string): Record<string, unknown>[] {
    return this.calls.filter((c) => c.method === method).map((c) => c.params);
  }

  /** Drops every connection (the daemon restarted). */
  drop(): void {
    for (const conn of this.#conns) conn.destroy();
  }

  /** Pushes a notification to every connection. */
  notify<N extends AgentNotificationName>(method: N, params: AgentNotifications[N]): void {
    for (const conn of this.#conns) conn.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n');
  }

  async stop(): Promise<void> {
    this.drop();
    await new Promise<void>((resolve) => this.#server.close(() => resolve()));
    rmSync(this.#dir, { recursive: true, force: true });
  }

  #serve(conn: Socket): void {
    this.#conns.add(conn);
    conn.on('close', () => this.#conns.delete(conn));
    conn.on('error', () => {});
    let buffer = '';
    let hello = false;
    conn.setEncoding('utf8');
    conn.on('data', (chunk: string) => {
      buffer += chunk;
      let end: number;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const msg = JSON.parse(buffer.slice(0, end));
        buffer = buffer.slice(end + 1);
        const reply = (body: Reply) => {
          if (!conn.destroyed) conn.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, ...body }) + '\n');
        };
        if (!hello) {
          if (msg.method !== 'hello' || msg.params.token !== this.token) {
            reply({ error: { code: -32100, message: 'wrong or outdated agent token', data: { kind: 'unauthenticated' } } });
            continue;
          }
          hello = true;
          this.#hellos += 1;
          reply({ result: { daemon_version: '0.0.0', protocol_version: { major: 1, minor: 0 }, session_id: 'S', capabilities: ['remote_tools'] } });
          for (const wake of this.#waiters.splice(0)) wake();
          continue;
        }
        this.calls.push({ method: msg.method, params: msg.params });
        if (this.silent.has(msg.method)) continue;
        const handler = this.handlers.get(msg.method);
        if (!handler) {
          reply({ error: { code: -32601, message: `unknown method ${msg.method}`, data: { kind: 'method_not_found' } } });
          continue;
        }
        void Promise.resolve(handler(msg.params)).then(reply);
      }
    });
  }
}

/** A todo entry for tests. */
export function todo(id: string, text: string, status: TodoStatus = 'pending'): Todo {
  return { id, text, status, reason: null, updated_by: { kind: 'agent' }, updated_at: 1 };
}

/** A connected, opened workspace on `machine` at `~/<dir>`, alice's home `/home/alice`. */
export function workspace(machine: string, dir: string, extra: Partial<WorkspaceInfo> = {}): WorkspaceInfo {
  return {
    machine,
    path: `~/${dir}`,
    root: `/home/alice/${dir}`,
    home: '/home/alice',
    primary: false,
    shell: machine,
    availability: { connectivity: { state: 'connected' }, since: 1 },
    context_files: [],
    skills: [],
    ...extra,
  };
}

/** An idle shell. */
export function shell(name: string, machine: string, cwd: string): AgentShell {
  return { name, machine, workspace_id: `W-${machine}`, cwd, state: { state: 'idle' }, reset_pending: false, foreground: null };
}

/** One workspace on devbox (`~/src/api`) with its default shell. */
export const ONE: SessionInfo = {
  workspaces: [workspace('devbox', 'src/api', { primary: true })],
  shells: [shell('devbox', 'devbox', '/home/alice/src/api')],
};

/** devbox (`~/src/api`, primary) and buildbox (`~/work/web`). */
export const TWO: SessionInfo = {
  workspaces: [workspace('devbox', 'src/api', { primary: true }), workspace('buildbox', 'work/web')],
  shells: [shell('devbox', 'devbox', '/home/alice/src/api'), shell('buildbox', 'buildbox', '/tmp')],
};

/** A `host.call` handler from one per op: `{op, ...}` params in, a `HostResult` (or an error) out. */
export function hostOps(
  ops: Record<string, (op: Record<string, unknown>, params: Record<string, unknown>) => Record<string, unknown> | Reply | Promise<Reply>>,
  linkDrops = 0,
): Handler {
  return async (params) => {
    const op = params.op as Record<string, unknown>;
    const serve = ops[op.op as string];
    if (!serve) return rpcError('unsupported', {}, `no ${String(op.op)} here`);
    const out = await serve(op, params);
    if ('error' in out || 'result' in out) {
      const reply = out as Reply;
      return reply.error ? reply : { result: { result: { op: op.op, ...(reply.result as object) }, link_drops: linkDrops } };
    }
    return { result: { result: { op: op.op, ...out }, link_drops: linkDrops } };
  };
}

/** A JSON-RPC error reply with a typed error. */
export function rpcError(kind: string, data: Record<string, unknown> = {}, message = kind): Reply {
  return { error: { code: -32000, message, data: { kind, ...data } } };
}
