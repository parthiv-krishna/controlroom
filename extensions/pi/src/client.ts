// The agent API client: newline-delimited JSON-RPC over the session's agent socket.
//
// - `start()` connects and says `hello` with the session's token; a dropped connection is
//   retried with backoff, forever, until `close()`. The daemon may restart at any time.
// - `call()` waits for the connection, up to the call's timeout (none for host calls, which
//   the daemon keeps open while a host is unreachable). Calls in flight when the connection
//   drops are sent again once it is back: mutating calls carry the tool call id as an
//   idempotency key, so the daemon applies them once. An aborted call stops waiting; a late
//   answer is ignored.
// - A wrong token or an incompatible protocol is final: every call fails with that error.
// - The daemon pushes notifications (`state`, `session.info`, `host.output`, …); `on()`
//   listeners get them, and the latest `state` and `session.info` are kept.

import { createConnection, type Socket } from 'node:net';

import type { AgentMethod, AgentMethods, AgentNotificationName, AgentNotifications } from './protocol/AgentMethods.ts';
import type { AgentState } from './protocol/AgentState.ts';
import type { ErrorData } from './protocol/ErrorData.ts';
import type { RpcError } from './protocol/RpcError.ts';
import type { SessionInfo } from './protocol/SessionInfo.ts';
import { AGENT_PROTOCOL_VERSION } from './protocol/versions.ts';

/** A failed call, with the daemon's typed error when it sent one. */
export class ControlroomError extends Error {
  readonly kind: string;
  /** The typed error, when the daemon sent one. */
  readonly data: ErrorData | null;

  constructor(message: string, kind: string, data: ErrorData | null = null) {
    super(message);
    this.name = 'ControlroomError';
    this.kind = kind;
    this.data = data;
  }
}

/** Client settings. */
export interface ClientOptions {
  /** The session's agent socket. */
  socket: string;
  /** The launch's token. */
  token: string;
  /** Sent in `hello`, for the daemon's log. */
  clientName: string;
  /** How long a call may wait for the connection and its answer by default. Default 30 s. */
  callTimeoutMs?: number;
  /** First and longest reconnect delays. Default 200 ms and 5 s. */
  retryMs?: { first: number; max: number };
  /** Opens the socket (tests replace it). */
  connect?: (path: string) => Socket;
}

/** Per-call settings. */
export interface CallOptions {
  /** Overrides the default timeout; `null` waits as long as it takes. */
  timeoutMs?: number | null;
  /** Stops waiting (the call fails with kind `aborted`). */
  signal?: AbortSignal;
}

interface Pending {
  method: string;
  params: unknown;
  sent: boolean;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  /** Clears the timer and the abort listener. */
  done: () => void;
}

/** Error kinds that make reconnecting pointless. */
const FINAL = new Set(['unauthenticated', 'incompatible_version']);

type Listener<N extends AgentNotificationName> = (params: AgentNotifications[N]) => void;

/** The client of one session's agent socket. */
export class AgentClient {
  #options: Required<Omit<ClientOptions, 'connect'>> & Pick<ClientOptions, 'connect'>;
  #socket: Socket | null = null;
  #ready = false;
  #closed = true;
  #final: ControlroomError | null = null;
  #buffer = '';
  #nextId = 1;
  #helloId = 0;
  #pending = new Map<number, Pending>();
  #attempt = 0;
  #retry: NodeJS.Timeout | null = null;
  #listeners = new Map<string, Set<(params: never) => void>>();
  #state: AgentState | null = null;
  #info: SessionInfo | null = null;

  constructor(options: ClientOptions) {
    this.#options = {
      callTimeoutMs: 30_000,
      retryMs: { first: 200, max: 5_000 },
      ...options,
    };
  }

  /** The latest state the daemon pushed, if any. */
  get state(): AgentState | null {
    return this.#state;
  }

  /** The latest session info the daemon pushed, if any. */
  get info(): SessionInfo | null {
    return this.#info;
  }

  /** Whether the handshake succeeded on the current connection. */
  get connected(): boolean {
    return this.#ready;
  }

  /** Connects (and keeps reconnecting) until `close()`. */
  start(): void {
    if (!this.#closed) return;
    this.#closed = false;
    this.#connect();
  }

  /** Disconnects for good; pending calls fail. */
  close(): void {
    this.#closed = true;
    if (this.#retry) clearTimeout(this.#retry);
    this.#retry = null;
    this.#socket?.destroy();
    this.#socket = null;
    this.#ready = false;
    this.#failAll(new ControlroomError('the controlroom connection was closed', 'closed'));
  }

  /** Subscribes to a notification; returns the unsubscribe function. */
  on<N extends AgentNotificationName>(method: N, listener: Listener<N>): () => void {
    let set = this.#listeners.get(method);
    if (!set) this.#listeners.set(method, (set = new Set()));
    const entry = listener as (params: never) => void;
    set.add(entry);
    return () => set.delete(entry);
  }

  /** The session's info: the latest pushed, or fetched now. */
  async sessionInfo(): Promise<SessionInfo> {
    return this.#info ?? (await this.call('session.info', {}));
  }

  /** Calls an agent API method. */
  call<M extends Exclude<AgentMethod, 'hello'>>(
    method: M,
    params: AgentMethods[M]['params'],
    options: CallOptions = {},
  ): Promise<AgentMethods[M]['result']> {
    if (this.#final) return Promise.reject(this.#final);
    if (this.#closed) return Promise.reject(new ControlroomError('the controlroom connection is closed', 'closed'));
    const { signal } = options;
    if (signal?.aborted) return Promise.reject(aborted());
    return new Promise((resolve, reject) => {
      const id = this.#nextId++;
      const fail = (error: Error) => {
        this.#pending.delete(id);
        pending.done();
        reject(error);
      };
      const timeoutMs = options.timeoutMs === undefined ? this.#options.callTimeoutMs : options.timeoutMs;
      const timer =
        timeoutMs === null
          ? null
          : setTimeout(() => {
              const why = this.#ready ? 'did not answer' : 'is not reachable';
              fail(new ControlroomError(`the controlroom daemon ${why} (${method})`, 'timeout'));
            }, timeoutMs);
      timer?.unref();
      const onAbort = () => fail(aborted());
      signal?.addEventListener('abort', onAbort, { once: true });
      const pending: Pending = {
        method,
        params,
        sent: false,
        resolve: resolve as (v: unknown) => void,
        reject,
        done: () => {
          if (timer) clearTimeout(timer);
          signal?.removeEventListener('abort', onAbort);
        },
      };
      this.#pending.set(id, pending);
      if (this.#ready) this.#send(id, pending);
    });
  }

  #connect(): void {
    if (this.#closed) return;
    const socket = (this.#options.connect ?? createConnection)(this.#options.socket);
    this.#socket = socket;
    this.#buffer = '';
    socket.setEncoding('utf8');
    socket.unref();
    socket.on('connect', () => this.#hello());
    socket.on('data', (chunk: string) => this.#receive(chunk));
    socket.on('error', () => {});
    socket.on('close', () => this.#lost(socket));
  }

  #hello(): void {
    this.#helloId = this.#nextId++;
    this.#write({
      jsonrpc: '2.0',
      id: this.#helloId,
      method: 'hello',
      params: { protocol_version: AGENT_PROTOCOL_VERSION, token: this.#options.token, client_name: this.#options.clientName },
    });
  }

  #receive(chunk: string): void {
    this.#buffer += chunk;
    let end: number;
    while ((end = this.#buffer.indexOf('\n')) >= 0) {
      const line = this.#buffer.slice(0, end).trim();
      this.#buffer = this.#buffer.slice(end + 1);
      if (line) this.#message(line);
    }
  }

  #message(line: string): void {
    let msg: { id?: number; result?: unknown; error?: RpcError; method?: string; params?: unknown };
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if (typeof msg.method === 'string' && msg.id === undefined) return this.#notification(msg.method, msg.params);
    if (typeof msg.id !== 'number') return;
    if (msg.id === this.#helloId) return this.#helloAnswered(msg.error);
    const pending = this.#pending.get(msg.id);
    if (!pending) return;
    this.#pending.delete(msg.id);
    pending.done();
    if (msg.error) pending.reject(toError(msg.error));
    else pending.resolve(msg.result);
  }

  #notification(method: string, params: unknown): void {
    if (params === undefined || params === null) return;
    if (method === 'state') this.#state = params as AgentState;
    if (method === 'session.info') this.#info = params as SessionInfo;
    for (const listener of this.#listeners.get(method) ?? []) listener(params as never);
  }

  #helloAnswered(error: RpcError | undefined): void {
    if (error) {
      const failure = toError(error);
      if (FINAL.has(failure.kind)) {
        this.#final = failure;
        this.#failAll(failure);
        this.#closed = true;
      }
      this.#socket?.destroy();
      return;
    }
    this.#ready = true;
    this.#attempt = 0;
    for (const [id, pending] of this.#pending) if (!pending.sent) this.#send(id, pending);
  }

  #send(id: number, pending: Pending): void {
    pending.sent = true;
    this.#write({ jsonrpc: '2.0', id, method: pending.method, params: pending.params });
  }

  #write(message: unknown): void {
    this.#socket?.write(JSON.stringify(message) + '\n');
  }

  #lost(socket: Socket): void {
    if (socket !== this.#socket) return;
    this.#socket = null;
    this.#ready = false;
    // Sent again after the next hello (idempotent by call id).
    for (const pending of this.#pending.values()) pending.sent = false;
    if (this.#closed) return;
    const { first, max } = this.#options.retryMs;
    const delay = Math.min(max, first * 2 ** this.#attempt++);
    this.#retry = setTimeout(() => this.#connect(), delay);
    this.#retry.unref();
  }

  #failAll(error: Error): void {
    for (const pending of this.#pending.values()) {
      pending.done();
      pending.reject(error);
    }
    this.#pending.clear();
  }
}

function aborted(): ControlroomError {
  return new ControlroomError('Operation aborted', 'aborted');
}

function toError(error: RpcError): ControlroomError {
  const data = error.data ?? null;
  return new ControlroomError(`controlroom: ${error.message}`, data?.kind ?? 'unknown', data);
}
