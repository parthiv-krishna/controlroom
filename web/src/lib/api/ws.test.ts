import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
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
  reply(index: number, body: object) {
    const { id } = JSON.parse(this.sent[index]);
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: '2.0', id, ...body }) });
  }
  push(method: string, params: unknown) {
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: '2.0', method, params }) });
  }
}
const last = () => FakeSocket.all[FakeSocket.all.length - 1];
const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status });
const signedIn = vi.fn(async () => json(200, { csrf_token: 'c1' })) as unknown as typeof fetch;
/** Lets the session check and the socket creation run. */
const settle = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
};

function make(extra: Partial<WsOptions> = {}) {
  return new WebSocketTransport({
    base: 'http://h/',
    WebSocketImpl: FakeSocket as unknown as typeof WebSocket,
    fetchImpl: signedIn,
    minBackoffMs: 10,
    maxBackoffMs: 40,
    heartbeatMs: 1_000,
    heartbeatTimeoutMs: 200,
    watchPage: false,
    ...extra,
  });
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
    const call = t.request('agents.list', { machine_id: 'm1' });
    last().open();
    expect(t.state).toBe('open');
    const sent = JSON.parse(last().sent[0]);
    expect(sent).toMatchObject({ jsonrpc: '2.0', method: 'agents.list', params: { machine_id: 'm1' } });
    last().reply(0, { result: { agents: [] } });
    await expect(call).resolves.toEqual({ agents: [] });
  });

  it('rejects typed errors, tolerates malformed ones and dispatches notifications', async () => {
    const t = make();
    await settle();
    last().open();
    const got: unknown[] = [];
    t.subscribe('state.events', (p) => got.push(p));
    last().push('state.events', { subscription_id: 's', changes: [] });
    expect(got).toEqual([{ subscription_id: 's', changes: [] }]);

    const a = t.request('keys.list', {});
    last().reply(0, { error: { code: -32100, message: 'no', data: { kind: 'unauthenticated' } } });
    await expect(a).rejects.toMatchObject({ code: -32100, data: { kind: 'unauthenticated' } });

    const b = t.request('keys.list', {});
    last().reply(1, { error: 'garbage' });
    await expect(b).rejects.toMatchObject({ data: { kind: 'unknown' } });
  });

  it('stops on 401 and reports the login URL', async () => {
    const t = make({ fetchImpl: (async () => json(401, { login_url: '/auth/login' })) as unknown as typeof fetch });
    await settle();
    expect(t.auth).toEqual({ status: 'login', login_url: 'http://h/auth/login' });
    expect(FakeSocket.all).toHaveLength(0);
    expect(t.state).toBe('closed');
  });

  it('flushes only calls still pending when the socket opens', async () => {
    const t = make();
    await settle();
    const quick = t.request('keys.list', {}, { timeoutMs: 50 });
    const quickFailed = expect(quick).rejects.toBeInstanceOf(RpcError);
    const slow = t.request('agents.list', { machine_id: 'm1' }, { timeoutMs: 5_000 });
    await vi.advanceTimersByTimeAsync(60);
    await quickFailed;
    last().open();
    expect(last().sent.map((s) => JSON.parse(s).method)).toEqual(['agents.list']);
    last().reply(0, { result: { agents: [] } });
    await expect(slow).resolves.toEqual({ agents: [] });
  });

  it('fails queued and in-flight calls on close and never replays them', async () => {
    const t = make();
    await settle();
    last().open();
    const inFlight = t.request('keys.list', {});
    last().drop();
    await expect(inFlight).rejects.toBeInstanceOf(ConnectionError);
    expect(t.state).toBe('closed');
    // while closed, calls fail at once
    await expect(t.request('keys.list', {})).rejects.toMatchObject({ reason: 'offline' });
    // reconnect: queued-while-connecting call is sent, the failed one is not
    await vi.advanceTimersByTimeAsync(10);
    await settle();
    const queued = t.request('agents.list', { machine_id: 'm1' });
    last().open();
    expect(last().sent.map((s) => JSON.parse(s).method)).toEqual(['agents.list']);
    last().reply(0, { result: { agents: [] } });
    await expect(queued).resolves.toEqual({ agents: [] });
  });

  it('drops a late response after a timeout and reconnects (half-open socket)', async () => {
    const t = make();
    await settle();
    last().open();
    const first = last();
    const call = t.request('keys.list', {}, { timeoutMs: 100 });
    const timedOut = expect(call).rejects.toMatchObject({ data: { kind: 'timeout' } });
    await vi.advanceTimersByTimeAsync(100);
    await timedOut;
    expect(first.closed).toBe(true);
    expect(t.state).toBe('closed');
    first.reply(0, { result: {} }); // ignored
    await vi.advanceTimersByTimeAsync(10);
    await settle();
    expect(FakeSocket.all).toHaveLength(2);
  });

  it('sends a heartbeat and reconnects when it is not answered', async () => {
    const t = make();
    await settle();
    last().open();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(JSON.parse(last().sent[0]).method).toBe('notifications.preferences');
    last().reply(0, { result: { attention: true, questions: false, turn_end: false } });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().sent).toHaveLength(2);
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

  it('close() fails pending calls and stops reconnecting', async () => {
    const t = make();
    await settle();
    last().open();
    const call = t.request('keys.list', {});
    t.close();
    await expect(call).rejects.toMatchObject({ reason: 'closed' });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(FakeSocket.all).toHaveLength(1);
  });

  it('builds file URLs for the reserved resource channel', () => {
    const t = make();
    expect(t.resourceUrl('m 1', '~/src/a b.md')).toBe('http://h/api/files/m%201/~/src/a%20b.md');
    t.close();
  });
});
