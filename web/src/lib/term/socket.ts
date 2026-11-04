// The production terminal stream: a binary WebSocket to `api/stream/terminal/<session>/<shell>`
// (crates/protocol terminal.rs). Binary frames carry terminal bytes both ways; text frames carry
// a TerminalCommand (to the server) or a TerminalEvent (from it).
//
// - Unexpected closes reconnect with exponential backoff; every (re)connect starts with a fresh
//   replay (`attached {reset: true}`), and the last size is sent again.
// - `ended` and close code 4401 (signed out) stop reconnecting until reconnect().
import type { TerminalCommand, TerminalEvent } from '../protocol';
import type { LinkStatus, TerminalHandlers, TerminalLink, TerminalTarget } from './link';

/** The server's close code when the login is gone. */
export const CLOSE_UNAUTHENTICATED = 4401;

export interface SocketOptions {
  WebSocketImpl?: typeof WebSocket;
  minBackoffMs?: number;
  maxBackoffMs?: number;
}

/** The stream's URL under the SPA root `base`. */
export function terminalUrl(base: string, target: TerminalTarget): string {
  const path = `api/stream/terminal/${encodeURIComponent(target.session)}/${encodeURIComponent(target.shell)}`;
  return `${base.replace(/^http/, 'ws')}${path}`;
}

/** Opens the stream; it connects at once. */
export function openTerminalSocket(base: string, target: TerminalTarget, handlers: TerminalHandlers, options: SocketOptions = {}): TerminalLink {
  const WS = options.WebSocketImpl ?? WebSocket;
  const minBackoff = options.minBackoffMs ?? 500;
  const maxBackoff = options.maxBackoffMs ?? 10_000;
  const url = terminalUrl(base, target);
  const encoder = new TextEncoder();
  let socket: WebSocket | null = null;
  let live = false;
  let closed = false;
  let size: [number, number] | null = null;
  let backoff = minBackoff;
  let retry: ReturnType<typeof setTimeout> | null = null;

  const status = (s: LinkStatus) => {
    live = s.state === 'live';
    handlers.status(s);
  };
  const command = (c: TerminalCommand) => {
    if (socket?.readyState === WS.OPEN) socket.send(JSON.stringify(c));
  };

  function event(e: TerminalEvent): boolean {
    switch (e.kind) {
      case 'attached':
        if (e.reset) handlers.reset();
        backoff = minBackoff;
        status({ state: 'live' });
        return false;
      case 'detached':
        status({ state: 'detached', reason: e.reason });
        return false;
      case 'foreground':
        handlers.foreground(e.name);
        return false;
      case 'ended':
        status({ state: 'ended', reason: e.reason });
        return true;
      default:
        return false;
    }
  }

  function connect(): void {
    retry = null;
    status({ state: 'connecting' });
    const ws = new WS(url);
    ws.binaryType = 'arraybuffer';
    socket = ws;
    let ended = false;
    ws.onopen = () => {
      if (size) command({ kind: 'resize', cols: size[0], rows: size[1] });
    };
    ws.onmessage = (m: MessageEvent) => {
      if (typeof m.data === 'string') {
        try {
          ended = event(JSON.parse(m.data) as TerminalEvent) || ended;
        } catch {
          // not an event: ignore it
        }
      } else {
        // binaryType is 'arraybuffer': every other frame is output.
        handlers.output(new Uint8Array(m.data as ArrayBuffer));
      }
    };
    ws.onclose = (e: CloseEvent) => {
      if (socket !== ws) return;
      socket = null;
      live = false;
      if (closed || ended) return;
      if (e.code === CLOSE_UNAUTHENTICATED) return status({ state: 'signed_out' });
      status({ state: 'offline' });
      retry = setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, maxBackoff);
    };
  }

  connect();
  return {
    input(data) {
      if (live && socket?.readyState === WS.OPEN) socket.send(typeof data === 'string' ? encoder.encode(data) : data);
    },
    resize(cols, rows) {
      size = [cols, rows];
      command({ kind: 'resize', cols, rows });
    },
    reconnect() {
      if (closed || socket || retry) return;
      backoff = minBackoff;
      connect();
    },
    close() {
      closed = true;
      if (retry) clearTimeout(retry);
      const ws = socket;
      socket = null;
      ws?.close(1000);
    },
  };
}
