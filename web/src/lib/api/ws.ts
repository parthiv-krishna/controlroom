// JSON-RPC 2.0 over one same-origin WebSocket per tab (UI API, spec §5.4, §13).
//
// - Before each connect, GET auth/session: 200 → signed in, 401 → login (stop reconnecting).
// - Calls made while connecting wait in the offline queue; on open only calls still pending are
//   sent (a call that timed out is gone). On close every pending call fails and the queue is
//   cleared, so nothing is replayed after its caller was told it failed. While closed, calls fail
//   immediately.
// - Every call has a timeout; a late response is dropped.
// - Heartbeat: a cheap call every `heartbeatMs` while open, and on return to a visible page. A
//   missed heartbeat (or any timed-out call) means the socket is half-open: drop it and reconnect.
// - Reconnects with exponential backoff.
import type { MachineId, RemotePath, UiMethod, UiNotificationName, UiNotifications } from '../protocol';
import { ConnectionError, RpcError } from './errors';
import type { TerminalHandlers, TerminalLink, TerminalTarget } from '../term/link';
import { openTerminalSocket } from '../term/socket';
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
  /** Probe on visibilitychange/online (off in unit tests). */
  watchPage?: boolean;
}

interface Pending {
  method: UiMethod;
  text: string;
  sent: boolean;
  resolve: (value: never) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/** The method used as heartbeat: a cheap read every server answers. */
const HEARTBEAT_METHOD = 'notifications.preferences';

export class WebSocketTransport implements Transport {
  state: ConnState = 'connecting';
  auth: AuthState = { status: 'unknown' };

  private readonly hub = new NotificationHub();
  private readonly stateListeners = new Listeners<ConnState>();
  private readonly authListeners = new Listeners<AuthState>();
  private readonly pending = new Map<number, Pending>();
  private readonly options: Required<Omit<WsOptions, 'WebSocketImpl' | 'fetchImpl'>>;
  private readonly WebSocketImpl: typeof WebSocket;
  private readonly fetchImpl: typeof fetch;
  private ws: WebSocket | null = null;
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
      watchPage: options.watchPage ?? true,
    };
    this.WebSocketImpl = options.WebSocketImpl ?? WebSocket;
    this.fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
    this.backoff = this.options.minBackoffMs;
    this.detachPage = this.options.watchPage ? this.watchPage() : () => {};
    void this.connect();
  }

  request<M extends UiMethod>(method: M, params: Params<M>, options: CallOptions = {}): Promise<Result<M>> {
    if (this.state === 'closed') return Promise.reject(new ConnectionError('offline'));
    const id = this.nextId++;
    const text = JSON.stringify({ jsonrpc: '2.0', id, method, params });
    return new Promise<Result<M>>((resolve, reject) => {
      const timer = setTimeout(() => this.expire(id), options.timeoutMs ?? timeoutFor(method));
      const entry: Pending = { method, text, sent: false, resolve: resolve as (value: never) => void, reject, timer };
      this.pending.set(id, entry);
      if (this.state === 'open') this.send(entry);
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

  /** Probes the socket now (e.g. after the page was in the background). */
  checkAlive(): void {
    if (this.state !== 'open') return;
    this.request(HEARTBEAT_METHOD, {}, { timeoutMs: this.options.heartbeatTimeoutMs }).catch(() => {
      // a timeout already dropped the socket (see expire); other errors mean it answered
    });
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
    this.dropSocket(new ConnectionError('closed'));
    void this.connect();
  }

  openTerminal(target: TerminalTarget, handlers: TerminalHandlers): TerminalLink {
    return openTerminalSocket(this.options.base, target, handlers, { WebSocketImpl: this.WebSocketImpl });
  }
  resourceUrl(machineId: MachineId, path: RemotePath): string {
    return fileUrl(this.options.base, machineId, path);
  }

  close(): void {
    this.closedByUser = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.detachPage();
    this.dropSocket(new ConnectionError('closed'));
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
    this.setState('connecting');
    const session = await this.checkSession();
    if (this.closedByUser) return;
    if (session !== 'ok') {
      this.failAll(new ConnectionError('offline'));
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
      for (const entry of this.pending.values()) if (!entry.sent) this.send(entry);
      this.heartbeatTimer = setInterval(() => this.checkAlive(), this.options.heartbeatMs);
    };
    ws.onmessage = (event) => {
      if (this.ws === ws && typeof event.data === 'string') this.receive(event.data);
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.dropSocket(new ConnectionError('lost'));
      if (!this.closedByUser) this.scheduleReconnect();
    };
    ws.onerror = () => {
      // onclose follows
    };
  }

  /** Forgets the socket and fails every pending call (sent or still queued). */
  private dropSocket(error: ConnectionError): void {
    const ws = this.ws;
    this.ws = null;
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = null;
    if (ws) {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      ws.close();
    }
    this.failAll(error);
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

  private send(entry: Pending): void {
    entry.sent = true;
    this.ws?.send(entry.text);
  }

  private expire(id: number): void {
    const entry = this.pending.get(id);
    if (!entry) return;
    this.pending.delete(id);
    entry.reject(RpcError.timeout(entry.method));
    // A sent call without an answer: the socket may be half-open (e.g. after an iOS suspend).
    if (entry.sent && this.state === 'open') {
      this.dropSocket(new ConnectionError('lost'));
      if (!this.closedByUser) this.scheduleReconnect();
    }
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
    if ('error' in msg && msg.error !== undefined) entry.reject(RpcError.fromWire(msg.error));
    else entry.resolve(msg.result as never);
  }

  private watchPage(): () => void {
    if (typeof document === 'undefined') return () => {};
    const probe = () => {
      if (document.visibilityState === 'visible') this.checkAlive();
    };
    document.addEventListener('visibilitychange', probe);
    window.addEventListener('online', probe);
    return () => {
      document.removeEventListener('visibilitychange', probe);
      window.removeEventListener('online', probe);
    };
  }
}
