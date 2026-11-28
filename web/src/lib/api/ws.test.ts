import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { UI_PROTOCOL_VERSION } from '../protocol';
import { ConnectionError, RpcError } from './errors';
import { WebSocketTransport, type WsOptions } from './ws';

class FakeSocket {
  static all: FakeSocket[] = [];
  sent: string[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    FakeSocket.all.push(this);
  }
  send(text: string) {
    this.sent.push(text);
  }
  close() {
    this.closed = true;
  }
  /** Test helpers: the server side. */
  open() {
    this.onopen?.();
  }
  drop() {
    this.onclose?.();
  }
  /** The methods sent, in order. */
  methods(): string[] {
    return this.sent.map((s) => JSON.parse(s).method);
  }
  /** Answers the first sent call of `method` not answered yet. */
  answer(method: string, body: object) {
    const index = this.sent.findIndex((s, i) => JSON.parse(s).method === method && !this.answered.has(i));
    if (index < 0) throw new Error(`${method} was not sent`);
    this.answered.add(index);
    this.reply(index, body);
  }
  reply(index: number, body: object) {
    const { id } = JSON.parse(this.sent[index]);
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: '2.0', id, ...body }) });
  }
  push(method: string, params: unknown) {
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: '2.0', method, params }) });
  }
  private readonly answered = new Set<number>();
}
const last = () => FakeSocket.all[FakeSocket.all.length - 1];
const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status });
const signedIn = vi.fn(async () => json(200, { csrf_token: 'c1' })) as unknown as typeof fetch;
/** Lets the session check and the socket creation run. */
const settle = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
};
const HELLO = { protocol_version: UI_PROTOCOL_VERSION, client_name: null, device_id: 'd1' };

function make(extra: Partial<WsOptions> = {}) {
  return new WebSocketTransport({
    base: 'http://h/',
    WebSocketImpl: FakeSocket as unknown as typeof WebSocket,
    fetchImpl: signedIn,
    minBackoffMs: 10,
    maxBackoffMs: 40,
    heartbeatMs: 1_000,
    heartbeatTimeoutMs: 200,
    wakeProbeMs: 100,
    watchPage: false,
    ...extra,
  });
}

/** Opens the current socket and completes the handshake on it, as the sync engine does. */
async function handshake(t: WebSocketTransport) {
  const hello = t.request('hello', HELLO);
  last().open();
  last().answer('hello', { result: {} });
  await hello;
}

/** Drops the socket and lets the transport reconnect (after its backoff). */
async function reconnect() {
  last().drop();
  await vi.advanceTimersByTimeAsync(40);
  await settle();
}

describe('WebSocketTransport', () => {
  beforeEach(() => {
    FakeSocket.all = [];
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it('checks the login session, connects and correlates responses', async () => {
    const t = make();
    await settle();
    expect(last().url).toBe('ws://h/api/ws');
    expect(t.auth).toEqual({ status: 'ok' });
    await handshake(t);
    expect(t.state).toBe('open');
    const call = t.request('fs.list', { machine_id: 'm1', path: '~' });
    const sent = JSON.parse(last().sent[1]);
    expect(sent).toMatchObject({ jsonrpc: '2.0', method: 'fs.list', params: { machine_id: 'm1', path: '~' } });
    last().answer('fs.list', { result: { agents: [] } });
    await expect(call).resolves.toEqual({ agents: [] });
  });

  it('holds everything but the handshake until hello is answered', async () => {
    const t = make();
    await settle();
    const early = t.request('keys.list', {});
    last().open();
    const ping = t.request('ping', {});
    const hello = t.request('hello', HELLO);
    expect(last().methods()).toEqual(['ping', 'hello']);
    last().answer('ping', { result: {} });
    await ping;
    last().answer('hello', { result: {} });
    await hello;
    expect(last().methods()).toEqual(['ping', 'hello', 'keys.list']);
    last().answer('keys.list', { result: { keys: [] } });
    await expect(early).resolves.toEqual({ keys: [] });
  });

  it('rejects typed errors, tolerates malformed ones and dispatches notifications', async () => {
    const t = make();
    await settle();
    await handshake(t);
    const got: unknown[] = [];
    t.subscribe('state.events', (p) => got.push(p));
    last().push('state.events', { subscription_id: 's', changes: [] });
    expect(got).toEqual([{ subscription_id: 's', changes: [] }]);

    const a = t.request('keys.list', {});
    last().answer('keys.list', { error: { code: -32100, message: 'no', data: { kind: 'unauthenticated' } } });
    await expect(a).rejects.toMatchObject({ code: -32100, data: { kind: 'unauthenticated' } });

    const b = t.request('keys.list', {});
    last().answer('keys.list', { error: 'garbage' });
    await expect(b).rejects.toMatchObject({ data: { kind: 'unknown' } });
  });

  it('stops on 401 and reports the login URL', async () => {
    const t = make({ fetchImpl: (async () => json(401, { login_url: '/auth/login' })) as unknown as typeof fetch });
    await settle();
    expect(t.auth).toEqual({ status: 'login', login_url: 'http://h/auth/login' });
    expect(FakeSocket.all).toHaveLength(0);
    expect(t.state).toBe('closed');
    await expect(t.request('keys.list', {})).rejects.toMatchObject({ reason: 'closed' });
  });

  it('a call that never left fails with offline when its timeout ends', async () => {
    const t = make();
    await settle();
    const quick = t.request('keys.list', {}, { timeoutMs: 50 });
    const quickFailed = expect(quick).rejects.toMatchObject({ reason: 'offline' });
    const slow = t.request('fs.list', { machine_id: 'm1', path: '~' }, { timeoutMs: 5_000 });
    await vi.advanceTimersByTimeAsync(60);
    await quickFailed;
    await handshake(t);
    expect(last().methods()).toEqual(['hello', 'fs.list']);
    last().answer('fs.list', { result: { agents: [] } });
    await expect(slow).resolves.toEqual({ agents: [] });
  });

  it('after a drop, resends reads and op_id commands, and fails the others with lost', async () => {
    const t = make();
    await settle();
    await handshake(t);
    const read = t.request('keys.list', {});
    const command = t.request('session.mark_unread', { op_id: 'op-1', id: 's1' });
    const other = t.request('tasks.delete', { task_id: 't1' });
    const otherFailed = expect(other).rejects.toMatchObject({ reason: 'lost' });
    expect(last().methods()).toEqual(['hello', 'keys.list', 'session.mark_unread', 'tasks.delete']);
    await reconnect();
    await otherFailed;
    expect(t.state).toBe('connecting');
    // Calls made meanwhile wait as well.
    const meanwhile = t.request('fs.list', { machine_id: 'm1', path: '~' });
    await handshake(t);
    expect(last().methods()).toEqual(['hello', 'keys.list', 'session.mark_unread', 'fs.list']);
    expect(JSON.parse(last().sent[2]).params.op_id).toBe('op-1');
    last().answer('keys.list', { result: { keys: [] } });
    last().answer('session.mark_unread', { result: { read_seq: 1, unread: true } });
    last().answer('fs.list', { result: { agents: [] } });
    await expect(read).resolves.toEqual({ keys: [] });
    await expect(command).resolves.toEqual({ read_seq: 1, unread: true });
    await expect(meanwhile).resolves.toEqual({ agents: [] });
  });

  it('subscriptions and the handshake belong to their connection: they fail with lost', async () => {
    const t = make();
    await settle();
    await handshake(t);
    const sub = t.request('state.subscribe', { since: null });
    const failed = expect(sub).rejects.toBeInstanceOf(ConnectionError);
    await reconnect();
    await failed;
    await handshake(t);
    expect(last().methods()).toEqual(['hello']);
  });

  it('while disconnected, queue: false fails at once and other calls wait', async () => {
    const t = make({ fetchImpl: (async () => Promise.reject(new TypeError('network'))) as unknown as typeof fetch });
    await settle();
    expect(t.state).toBe('closed');
    await expect(t.request('client.focus', { device_id: 'd1', session_id: null, visible: true }, { queue: false })).rejects.toMatchObject({ reason: 'offline' });
    let settled = false;
    void t.request('keys.list', {}).catch(() => (settled = true));
    await vi.advanceTimersByTimeAsync(1_000);
    expect(settled).toBe(false);
  });

  it('drops a late response after a timeout and reconnects (half-open socket)', async () => {
    const t = make();
    await settle();
    await handshake(t);
    const first = last();
    const call = t.request('keys.list', {}, { timeoutMs: 100 });
    const timedOut = expect(call).rejects.toMatchObject({ data: { kind: 'timeout' } });
    await vi.advanceTimersByTimeAsync(100);
    await timedOut;
    expect(first.closed).toBe(true);
    expect(t.state).toBe('closed');
    first.answer('keys.list', { result: {} }); // ignored
    await vi.advanceTimersByTimeAsync(10);
    await settle();
    expect(FakeSocket.all).toHaveLength(2);
  });

  it('pings as a heartbeat and reconnects when it is not answered', async () => {
    const t = make();
    await settle();
    await handshake(t);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().methods()).toEqual(['hello', 'ping']);
    last().answer('ping', { result: {} });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().methods()).toEqual(['hello', 'ping', 'ping']);
    await vi.advanceTimersByTimeAsync(200);
    expect(t.state).toBe('closed');
  });

  it('backs off exponentially while the server is unreachable', async () => {
    let up = false;
    const fetchImpl = vi.fn(async () => {
      if (!up) throw new TypeError('network');
      return json(200, { csrf_token: 'c' });
    });
    const t = make({ fetchImpl: fetchImpl as unknown as typeof fetch });
    await settle();
    expect(t.state).toBe('closed');
    await vi.advanceTimersByTimeAsync(10);
    await settle();
    await vi.advanceTimersByTimeAsync(20);
    await settle();
    expect(fetchImpl).toHaveBeenCalledTimes(3);
    up = true;
    await vi.advanceTimersByTimeAsync(40);
    await settle();
    last().open();
    expect(t.state).toBe('open');
  });

  it('wakes up: a closed connection reconnects at once, an open one is probed', async () => {
    let up = false;
    const fetchImpl = vi.fn(async () => {
      if (!up) throw new TypeError('network');
      return json(200, { csrf_token: 'c' });
    });
    const t = make({ fetchImpl: fetchImpl as unknown as typeof fetch, minBackoffMs: 60_000, maxBackoffMs: 60_000 });
    await settle();
    expect(t.state).toBe('closed');
    up = true;
    t.wake();
    await settle();
    expect(FakeSocket.all).toHaveLength(1);
    await handshake(t);
    t.wake();
    expect(last().methods()).toEqual(['hello', 'ping']);
    await vi.advanceTimersByTimeAsync(100);
    expect(t.state).toBe('closed');
  });

  it('close() fails pending calls and stops reconnecting', async () => {
    const t = make();
    await settle();
    await handshake(t);
    const call = t.request('keys.list', {});
    t.close();
    await expect(call).rejects.toMatchObject({ reason: 'closed' });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(FakeSocket.all).toHaveLength(1);
  });

  it('timeouts of answered calls are no errors', async () => {
    const t = make();
    await settle();
    await handshake(t);
    const call = t.request('keys.list', {}, { timeoutMs: 50 });
    last().answer('keys.list', { result: { keys: [] } });
    await expect(call).resolves.toEqual({ keys: [] });
    await vi.advanceTimersByTimeAsync(60);
    expect(t.state).toBe('open');
    expect(RpcError.timeout('x').data.kind).toBe('timeout');
  });

  it('builds file URLs for the reserved resource channel', () => {
    const t = make();
    expect(t.resourceUrl('m 1', '~/src/a b.md')).toBe('http://h/api/files/m%201/~/src/a%20b.md');
    t.close();
  });
});
