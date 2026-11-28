// JSON-RPC 2.0 over one same-origin WebSocket per tab (UI API, spec §5.4, §13).
//
// - Before each connect, GET auth/session: 200 → signed in, 401 → login (stop reconnecting).
// - Calls wait in one queue until the connection can take them: the socket is open and, but for
//   the handshake (`hello`, `ping`), `hello` was answered on it (the server refuses anything
//   else before). A call made while disconnected waits for the reconnect (`queue: false` fails
//   at once instead); one still unsent when its timeout ends fails with `offline`.
// - When the socket drops, calls already sent go out again on the next connection when that is
//   safe (reads and `op_id` commands, see policy.ts); the others fail with `lost`, since they may
//   have been applied. Nothing is sent after its caller was told it failed.
// - Every call has a timeout; a late response is dropped. A sent call without an answer means
//   the socket is half-open (e.g. after an iOS suspend): drop it and reconnect.
// - Heartbeat: a `ping` every `heartbeatMs` while open.
// - Reconnects with exponential backoff, and at once when the page comes back (visible again,
//   `pageshow`, `resume`, `online`): a closed connection reconnects now, an open one is probed.
//   Terminal streams are woken with it. (`freeze` needs nothing: no timer runs while frozen.)
import type { MachineId, RemotePath, UiMethod, UiNotificationName, UiNotifications } from '../protocol';
import { ConnectionError, RpcError } from './errors';
import type { TerminalHandlers, TerminalLink, TerminalTarget } from '../term/link';
import { openTerminalSocket } from '../term/socket';
import { HANDSHAKE, resendable } from './policy';
import { timeoutFor } from './timeouts';
import {
  Listeners,
  NotificationHub,
  fileUrl,
  type AuthState,
  type CallOptions,
  type ConnState,
  type Params,
  type Result,
  type Transport,
} from './transport';

export interface WsOptions {
  /** The socket URL (default: <base>api/ws). */
  url?: string;
  /** The SPA root, where auth/ and api/ live. */
  base: string;
  WebSocketImpl?: typeof WebSocket;
  fetchImpl?: typeof fetch;
  minBackoffMs?: number;
  maxBackoffMs?: number;
  heartbeatMs?: number;
  heartbeatTimeoutMs?: number;
  /** How long the probe on the page's return waits for an answer. */
  wakeProbeMs?: number;
  /** Wake on visibilitychange, pageshow, resume and online (off in unit tests). */
  watchPage?: boolean;
}

interface Pending {
  method: UiMethod;
  text: string;
  /** Sent again after a drop (see policy.ts). */
  resend: boolean;
  /** Sent on the current socket. */
  sent: boolean;
  resolve: (value: never) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class WebSocketTransport implements Transport {
  state: ConnState = 'connecting';
  auth: AuthState = { status: 'unknown' };

  private readonly hub = new NotificationHub();
  private readonly stateListeners = new Listeners<ConnState>();
  private readonly authListeners = new Listeners<AuthState>();
  /** Terminal streams skip their backoff when the page comes back. */
  private readonly wakeListeners = new Listeners<void>();
  private readonly pending = new Map<number, Pending>();
  private readonly options: Required<Omit<WsOptions, 'WebSocketImpl' | 'fetchImpl'>>;
  private readonly WebSocketImpl: typeof WebSocket;
  private readonly fetchImpl: typeof fetch;
  private ws: WebSocket | null = null;
  /** `hello` was answered on the current socket. */
  private ready = false;
  private nextId = 1;
  private backoff: number;
  private closedByUser = false;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private csrf: string | null = null;
  private readonly detachPage: () => void;

  constructor(options: WsOptions) {
    const wsUrl = options.url ?? `${options.base.replace(/^http/, 'ws')}api/ws`;
    this.options = {
      url: wsUrl,
      base: options.base,
      minBackoffMs: options.minBackoffMs ?? 500,
      maxBackoffMs: options.maxBackoffMs ?? 15_000,
      heartbeatMs: options.heartbeatMs ?? 25_000,
      heartbeatTimeoutMs: options.heartbeatTimeoutMs ?? 10_000,
      wakeProbeMs: options.wakeProbeMs ?? 4_000,
      watchPage: options.watchPage ?? true,
    };
    this.WebSocketImpl = options.WebSocketImpl ?? WebSocket;
    this.fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
    this.backoff = this.options.minBackoffMs;
    this.detachPage = this.options.watchPage ? this.watchPage() : () => {};
    void this.connect();
  }

  request<M extends UiMethod>(method: M, params: Params<M>, options: CallOptions = {}): Promise<Result<M>> {
    if (this.closedByUser || this.auth.status === 'login') return Promise.reject(new ConnectionError('closed'));
    if (this.state !== 'open' && options.queue === false) return Promise.reject(new ConnectionError('offline'));
    const id = this.nextId++;
    const text = JSON.stringify({ jsonrpc: '2.0', id, method, params });
    return new Promise<Result<M>>((resolve, reject) => {
      const timer = setTimeout(() => this.expire(id), options.timeoutMs ?? timeoutFor(method));
      const entry: Pending = { method, text, resend: resendable(method, params), sent: false, resolve: resolve as (value: never) => void, reject, timer };
      this.pending.set(id, entry);
      if (this.sendable(entry)) this.send(entry);
    });
  }

  subscribe<N extends UiNotificationName>(name: N, handler: (params: UiNotifications[N]) => void): () => void {
    return this.hub.subscribe(name, handler);
  }
  onState(handler: (state: ConnState) => void): () => void {
    return this.stateListeners.add(handler);
  }
  onAuth(handler: (auth: AuthState) => void): () => void {
    return this.authListeners.add(handler);
  }

  /**
   * The page is back (shown again, restored, resumed, online): a connection waiting out its
   * backoff reconnects now, an open one is probed (it may be half-open), and terminal streams
   * reconnect too.
   */
  wake(): void {
    if (this.closedByUser) return;
    this.wakeListeners.emit();
    if (this.state === 'open') {
      this.probe(this.options.wakeProbeMs);
    } else if (this.state === 'closed' && this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
      this.backoff = this.options.minBackoffMs;
      void this.connect();
    }
  }

  async logout(): Promise<void> {
    try {
      await this.fetchImpl(`${this.options.base}auth/logout`, {
        method: 'POST',
        credentials: 'same-origin',
        headers: this.csrf ? { 'X-CSRF-Token': this.csrf } : {},
      });
    } catch {
      // unreachable: the session check below decides what to show
    }
    this.csrf = null;
    this.dropSocket('closed');
    void this.connect();
  }

  openTerminal(target: TerminalTarget, handlers: TerminalHandlers): TerminalLink {
    return openTerminalSocket(this.options.base, target, handlers, {
      WebSocketImpl: this.WebSocketImpl,
      onWake: (wake) => this.wakeListeners.add(wake),
    });
  }
  resourceUrl(machineId: MachineId, path: RemotePath): string {
    return fileUrl(this.options.base, machineId, path);
  }

  close(): void {
    this.closedByUser = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.detachPage();
    this.dropSocket('closed');
  }

  // ---------------------------------------------------------------------------------------------

  private setState(state: ConnState): void {
    if (this.state === state) return;
    this.state = state;
    this.stateListeners.emit(state);
  }
  private setAuth(auth: AuthState): void {
    this.auth = auth;
    this.authListeners.emit(auth);
  }

  /** 'ok': connect; 'login': stop until the user signs in; 'retry': server unreachable. */
  private async checkSession(): Promise<'ok' | 'login' | 'retry'> {
    let res: Response;
    try {
      res = await this.fetchImpl(`${this.options.base}auth/session`, {
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { Accept: 'application/json' },
      });
    } catch {
      return 'retry';
    }
    if (res.status === 401) {
      const body = (await res.json().catch(() => ({}))) as { login_url?: string | null };
      this.setAuth({ status: 'login', login_url: body.login_url ? `${this.options.base}${body.login_url.replace(/^\.?\//, '')}` : null });
      return 'login';
    }
    if (!res.ok) return 'retry';
    const body = (await res.json().catch(() => null)) as { csrf_token?: string } | null;
    if (!body) return 'retry';
    this.csrf = body.csrf_token ?? null;
    this.setAuth({ status: 'ok' });
    return 'ok';
  }

  private async connect(): Promise<void> {
    this.retryTimer = null;
    this.setState('connecting');
    const session = await this.checkSession();
    if (this.closedByUser) return;
    if (session !== 'ok') {
      // Signed out: nothing waits for a connection that won't come until the user signs in.
      if (session === 'login') this.failAll(new ConnectionError('closed'));
      this.setState('closed');
      if (session === 'retry') this.scheduleReconnect();
      return;
    }
    const ws = new this.WebSocketImpl(this.options.url);
    this.ws = ws;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.backoff = this.options.minBackoffMs;
      this.setState('open');
      this.flush();
      this.heartbeatTimer = setInterval(() => this.probe(this.options.heartbeatTimeoutMs), this.options.heartbeatMs);
    };
    ws.onmessage = (event) => {
      if (this.ws === ws && typeof event.data === 'string') this.receive(event.data);
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.lose();
    };
    ws.onerror = () => {
      // onclose follows
    };
  }

  /** The connection dropped: keep what can be sent again, and reconnect. */
  private lose(): void {
    this.dropSocket('lost');
    if (!this.closedByUser) this.scheduleReconnect();
  }

  /**
   * Forgets the socket. `lost`: sent calls that are safe to repeat wait for the next connection
   * with the unsent ones, the others fail. `closed` (logout, close): every call fails.
   */
  private dropSocket(reason: 'lost' | 'closed'): void {
    const ws = this.ws;
    this.ws = null;
    this.ready = false;
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = null;
    if (ws) {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      ws.close();
    }
    if (reason === 'closed') {
      this.failAll(new ConnectionError('closed'));
    } else {
      for (const [id, entry] of this.pending) {
        if (!entry.sent) continue;
        if (entry.resend) {
          entry.sent = false;
        } else {
          this.pending.delete(id);
          clearTimeout(entry.timer);
          entry.reject(new ConnectionError('lost'));
        }
      }
    }
    this.setState('closed');
  }

  private failAll(error: Error): void {
    const all = [...this.pending.values()];
    this.pending.clear();
    for (const entry of all) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
  }

  private scheduleReconnect(): void {
    const wait = this.backoff;
    this.backoff = Math.min(this.backoff * 2, this.options.maxBackoffMs);
    this.retryTimer = setTimeout(() => void this.connect(), wait);
  }

  /** Whether the current connection can take the call now. */
  private sendable(entry: Pending): boolean {
    return this.state === 'open' && (this.ready || HANDSHAKE.has(entry.method));
  }

  /** Sends every waiting call the connection can take now, in call order. */
  private flush(): void {
    for (const entry of this.pending.values()) if (!entry.sent && this.sendable(entry)) this.send(entry);
  }

  private send(entry: Pending): void {
    entry.sent = true;
    this.ws?.send(entry.text);
  }

  /** A cheap call; a timeout drops the socket (see expire). */
  private probe(timeoutMs: number): void {
    this.request('ping', {}, { timeoutMs, queue: false }).catch(() => {
      // a timeout already dropped the socket; other errors mean it answered
    });
  }

  private expire(id: number): void {
    const entry = this.pending.get(id);
    if (!entry) return;
    this.pending.delete(id);
    // It never left: nothing happened on the server.
    if (!entry.sent) return entry.reject(new ConnectionError('offline'));
    entry.reject(RpcError.timeout(entry.method));
    // A sent call without an answer: the socket may be half-open (e.g. after an iOS suspend).
    if (this.state === 'open') this.lose();
  }

  private receive(raw: string): void {
    let msg: { id?: unknown; result?: unknown; error?: unknown; method?: unknown; params?: unknown };
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }
    if (!msg || typeof msg !== 'object') return;
    if (typeof msg.method === 'string' && msg.id === undefined) {
      this.hub.emit(msg.method, msg.params);
      return;
    }
    const entry = typeof msg.id === 'number' ? this.pending.get(msg.id) : undefined;
    if (!entry) return; // late (timed out) or unknown
    this.pending.delete(msg.id as number);
    clearTimeout(entry.timer);
    if ('error' in msg && msg.error !== undefined) return entry.reject(RpcError.fromWire(msg.error));
    entry.resolve(msg.result as never);
    if (entry.method === 'hello') {
      this.ready = true;
      this.flush();
    }
  }

  private watchPage(): () => void {
    if (typeof document === 'undefined') return () => {};
    const wake = () => this.wake();
    const shown = () => {
      if (document.visibilityState === 'visible') this.wake();
    };
    document.addEventListener('visibilitychange', shown);
    document.addEventListener('resume', wake);
    window.addEventListener('pageshow', wake);
    window.addEventListener('online', wake);
    return () => {
      document.removeEventListener('visibilitychange', shown);
      document.removeEventListener('resume', wake);
      window.removeEventListener('pageshow', wake);
      window.removeEventListener('online', wake);
    };
  }
}
