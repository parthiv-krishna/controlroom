// A scriptable stand-in for the daemon's agent socket: hello with a token, then canned
// handlers per method. Tests can drop connections and push state.

import { mkdtempSync, rmSync } from 'node:fs';
import { createServer, type Server, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import type { AgentState } from '../src/protocol/AgentState';
import type { Todo } from '../src/protocol/Todo';
import type { TodoStatus } from '../src/protocol/TodoStatus';

/** A method handler: a result, or an error object `{code, message, data}`. */
export type Handler = (params: Record<string, unknown>) => { result?: unknown; error?: unknown };

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

  /** Drops every connection (the daemon restarted). */
  drop(): void {
    for (const conn of this.#conns) conn.destroy();
  }

  /** Pushes a state notification to every connection. */
  push(state: AgentState): void {
    for (const conn of this.#conns) conn.write(JSON.stringify({ jsonrpc: '2.0', method: 'state', params: state }) + '\n');
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
        const reply = (body: object) => conn.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, ...body }) + '\n');
        if (!hello) {
          if (msg.method !== 'hello' || msg.params.token !== this.token) {
            reply({ error: { code: -32100, message: 'wrong or outdated agent token', data: { kind: 'unauthenticated' } } });
            continue;
          }
          hello = true;
          this.#hellos += 1;
          reply({ result: { daemon_version: '0.0.0', protocol_version: { major: 1, minor: 0 }, session_id: 'S', capabilities: [] } });
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
        reply(handler(msg.params));
      }
    });
  }
}

/** A todo entry for tests. */
export function todo(id: string, text: string, status: TodoStatus = 'pending'): Todo {
  return { id, text, status, reason: null, updated_by: { kind: 'agent' }, updated_at: 1 };
}
