// All backend traffic goes through a Transport; nothing else in the app touches WebSocket or fetch.
// Implementations: WebSocketTransport (the web server) and FixtureTransport (development and tests).
import type { MachineId, RemotePath, UiMethod, UiMethods, UiNotificationName, UiNotifications } from '../protocol';

/** Params and result of a UI API method. */
export type Params<M extends UiMethod> = UiMethods[M]['params'];
export type Result<M extends UiMethod> = UiMethods[M]['result'];

/** `open` after a reconnect means: say hello and resubscribe from the last cursors. */
export type ConnState = 'connecting' | 'open' | 'closed';

/** Whether the browser is signed in; `login` stops reconnecting until the user signs in. */
export type AuthState = { status: 'unknown' } | { status: 'ok' } | { status: 'login'; login_url: string | null };

export interface CallOptions {
  /** Overrides the method's default timeout. */
  timeoutMs?: number;
}

/** Reserved (spec §14/§19): duplex byte streams such as terminals. Not implemented yet. */
export type StreamKind = 'pty';
export interface DuplexStream {
  write(data: Uint8Array): void;
  onData(handler: (data: Uint8Array) => void): () => void;
  close(): void;
}

export interface Transport {
  readonly state: ConnState;
  readonly auth: AuthState;
  /** A call. Rejects with RpcError (incl. timeout) or ConnectionError; never hangs. */
  request<M extends UiMethod>(method: M, params: Params<M>, options?: CallOptions): Promise<Result<M>>;
  /** Server notifications (subscription feeds). Returns an unsubscribe function. */
  subscribe<N extends UiNotificationName>(name: N, handler: (params: UiNotifications[N]) => void): () => void;
  onState(handler: (state: ConnState) => void): () => void;
  onAuth(handler: (auth: AuthState) => void): () => void;
  /** Ends the login session. */
  logout(): Promise<void>;
  /** Reserved: a duplex stream (terminals, P5b). */
  openStream(kind: StreamKind, params: Record<string, unknown>): Promise<DuplexStream>;
  /** Reserved: an authenticated URL for a remote file (viewers, P7b). */
  resourceUrl(machineId: MachineId, path: RemotePath): string;
  close(): void;
}

/** Shared default for the reserved stream channel. */
export const streamsNotSupported = (): Promise<DuplexStream> => Promise.reject(new Error('streams are not supported yet'));

/** A remote file under `api/files/<machine>/<path>`; each POSIX segment is encoded, `~` is kept. */
export function fileUrl(base: string, machineId: MachineId, path: RemotePath): string {
  const segments = path.split('/').map((p) => (p === '~' ? p : encodeURIComponent(p)));
  return `${base}api/files/${encodeURIComponent(machineId)}/${segments.join('/')}`;
}

/** A typed set of handlers. */
export class Listeners<T> {
  private readonly handlers = new Set<(value: T) => void>();
  add(handler: (value: T) => void): () => void {
    this.handlers.add(handler);
    return () => this.handlers.delete(handler);
  }
  emit(value: T): void {
    for (const h of [...this.handlers]) h(value);
  }
}

/** Notification handlers by name. */
export class NotificationHub {
  private readonly byName = new Map<string, Listeners<unknown>>();
  subscribe<N extends UiNotificationName>(name: N, handler: (params: UiNotifications[N]) => void): () => void {
    let set = this.byName.get(name);
    if (!set) this.byName.set(name, (set = new Listeners()));
    return set.add(handler as (value: unknown) => void);
  }
  emit(name: string, params: unknown): void {
    this.byName.get(name)?.emit(params);
  }
}
