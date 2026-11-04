// Errors a call can fail with: a typed protocol error from the server (or a client-side timeout,
// which the protocol also reports as `timeout`), or a lost/absent connection.
import type { ErrorData, RpcError as WireError } from '../protocol';

/** A JSON-RPC error. `data` is the typed kind; unknown kinds decode as `{ kind: 'unknown' }`. */
export class RpcError extends Error {
  readonly code: number;
  readonly data: ErrorData;

  constructor(code: number, message: string, data: ErrorData | null | undefined) {
    super(message);
    this.name = 'RpcError';
    this.code = code;
    this.data = data && typeof data === 'object' && typeof data.kind === 'string' ? data : { kind: 'unknown' };
  }

  /** Decodes a wire error leniently: a malformed `error` member still fails its call. */
  static fromWire(raw: unknown): RpcError {
    const e = (raw && typeof raw === 'object' ? raw : {}) as Partial<WireError>;
    return new RpcError(typeof e.code === 'number' ? e.code : -32000, typeof e.message === 'string' ? e.message : 'error', e.data);
  }

  /** The client-side timeout of one call (protocol code -32004). */
  static timeout(method: string): RpcError {
    return new RpcError(-32004, `${method} timed out`, { kind: 'timeout' });
  }
}

/** The call never reached the server, or the connection dropped before its response. */
export class ConnectionError extends Error {
  constructor(readonly reason: 'offline' | 'lost' | 'closed') {
    super(reason === 'offline' ? 'Not connected' : reason === 'lost' ? 'Connection lost' : 'Connection closed');
    this.name = 'ConnectionError';
  }
}

/** A short human message for a failed call (toasts, inline errors). */
export function describeError(e: unknown): string {
  if (e instanceof ConnectionError) return e.message;
  if (!(e instanceof RpcError)) return e instanceof Error ? e.message : String(e);
  const d = e.data;
  switch (d.kind) {
    case 'timeout': return 'Timed out';
    case 'machine_unavailable': return `Machine ${d.connectivity.state.replace(/_/g, ' ')}`;
    case 'connection_lost': return 'The link to the machine dropped';
    case 'conflict': return `${d.path} changed meanwhile`;
    case 'shell_gone': return `The shell ${d.shell} is gone`;
    case 'already_resolved': return d.request.answer ? `Already answered on ${d.request.answer.by?.label ?? 'another device'}` : 'Already answered';
    case 'agent_unavailable': return `${d.agent}: ${d.detail}`;
    case 'sandbox_unavailable': return `Sandbox unavailable: ${d.detail}`;
    case 'fs_error': return `${d.path}: ${d.detail}`;
    case 'invalid_key': return `Invalid key: ${d.detail}`;
    case 'key_in_use': return 'The key is still used by a machine';
    case 'host_key_mismatch': return `Host key changed (now ${d.actual})`;
    case 'unsupported_platform': return `Unsupported platform: ${d.uname}`;
    case 'not_resumable': return "The agent can't resume this session";
    case 'invalid_state': return `Not possible while ${d.state.replace(/_/g, ' ')}`;
    case 'busy': return 'Busy, try again';
    case 'unauthenticated': return 'Signed out';
    default: return e.message;
  }
}
