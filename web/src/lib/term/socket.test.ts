import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LinkStatus, TerminalHandlers } from './link';
import { CLOSE_UNAUTHENTICATED, openTerminalSocket, terminalUrl } from './socket';

class FakeSocket {
  static readonly OPEN = 1;
  static all: FakeSocket[] = [];
  readyState = 0;
  binaryType = '';
  sent: (string | Uint8Array)[] = [];
  closedWith: number | null = null;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: ((e: { code: number }) => void) | null = null;
  constructor(public url: string) {
    FakeSocket.all.push(this);
  }
  send(data: string | Uint8Array) {
    this.sent.push(data);
  }
  close(code: number) {
    this.closedWith = code;
  }
  /** The server side. */
  open() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }
  event(e: object) {
    this.onmessage?.({ data: JSON.stringify(e) });
  }
  output(text: string) {
    this.onmessage?.({ data: new TextEncoder().encode(text).buffer });
  }
  drop(code = 1006) {
    this.readyState = 3;
    this.onclose?.({ code });
  }
  get commands(): unknown[] {
    return this.sent.filter((s) => typeof s === 'string').map((s) => JSON.parse(s as string));
  }
  get typed(): string[] {
    return this.sent.filter((s) => typeof s !== 'string').map((s) => new TextDecoder().decode(s as Uint8Array));
  }
}
const last = () => FakeSocket.all[FakeSocket.all.length - 1];

function handlers() {
  const log = { resets: 0, output: '', foreground: [] as (string | null)[], status: [] as LinkStatus[] };
  const h: TerminalHandlers = {
    reset: () => log.resets++,
    output: (d) => (log.output += new TextDecoder().decode(d)),
    foreground: (n) => log.foreground.push(n),
    status: (s) => log.status.push(s),
  };
  return { h, log, state: () => log.status[log.status.length - 1]?.state };
}

const open = (h: TerminalHandlers) =>
  openTerminalSocket('http://h/app/', { session: 's1', shell: 'dev box' }, h, {
    WebSocketImpl: FakeSocket as unknown as typeof WebSocket,
    minBackoffMs: 10,
    maxBackoffMs: 40,
  });

describe('terminal socket', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeSocket.all = [];
  });
  afterEach(() => vi.useRealTimers());

  it('connects to the shell under the SPA root', () => {
    expect(terminalUrl('https://h/app/', { session: 's1', shell: 'a/b c' })).toBe('wss://h/app/api/stream/terminal/s1/a%2Fb%20c');
    const { h } = handlers();
    open(h);
    expect(last().url).toBe('ws://h/app/api/stream/terminal/s1/dev%20box');
    expect(last().binaryType).toBe('arraybuffer');
  });

  it('streams output, resets on a fresh replay and types only while live', () => {
    const { h, log, state } = handlers();
    const link = open(h);
    expect(state()).toBe('connecting');
    link.resize(100, 30);
    last().open();
    expect(last().commands).toEqual([{ kind: 'resize', cols: 100, rows: 30 }]);
    link.input('early');
    last().event({ kind: 'attached', reset: true });
    expect(log.resets).toBe(1);
    expect(state()).toBe('live');
    last().output('$ ');
    link.input('ls\r');
    last().event({ kind: 'foreground', name: 'vim' });
    last().event({ kind: 'detached', reason: 'devbox is unreachable' });
    link.input('lost');
    last().event({ kind: 'attached', reset: false });
    last().event({ kind: 'bell' });
    expect(log.output).toBe('$ ');
    expect(last().typed).toEqual(['ls\r']);
    expect(log.foreground).toEqual(['vim']);
    expect(log.status.map((s) => s.state)).toEqual(['connecting', 'live', 'detached', 'live']);
    expect(log.resets).toBe(1);
  });

  it('reconnects after a drop with backoff and sends the size again', async () => {
    const { h, state } = handlers();
    const link = open(h);
    last().open();
    link.resize(80, 24);
    last().drop();
    expect(state()).toBe('offline');
    expect(FakeSocket.all).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(10);
    expect(FakeSocket.all).toHaveLength(2);
    last().open();
    expect(last().commands).toEqual([{ kind: 'resize', cols: 80, rows: 24 }]);
    last().drop();
    await vi.advanceTimersByTimeAsync(10);
    expect(FakeSocket.all).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(10);
    expect(FakeSocket.all).toHaveLength(3);
  });

  it('stops after ended or a sign-out until reconnect(), and close() ends it', async () => {
    const { h, state } = handlers();
    const link = open(h);
    last().open();
    last().event({ kind: 'ended', reason: 'the shell exited' });
    last().drop(1000);
    expect(state()).toBe('ended');
    await vi.advanceTimersByTimeAsync(100);
    expect(FakeSocket.all).toHaveLength(1);

    link.reconnect();
    expect(FakeSocket.all).toHaveLength(2);
    last().drop(CLOSE_UNAUTHENTICATED);
    expect(state()).toBe('signed_out');
    await vi.advanceTimersByTimeAsync(100);
    expect(FakeSocket.all).toHaveLength(2);

    link.reconnect();
    const ws = last();
    link.close();
    expect(ws.closedWith).toBe(1000);
    ws.drop(1000);
    await vi.advanceTimersByTimeAsync(100);
    expect(FakeSocket.all).toHaveLength(3);
  });
});

