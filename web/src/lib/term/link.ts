// A terminal stream (spec §22): one attached terminal on a session shell. The transport opens it
// (Transport.openTerminal): a WebSocket in production (socket.ts), a scripted fake shell in the
// fixture build (fixture.ts).
import type { SessionId } from '../protocol';

/** Which terminal: a shell of a session. */
export interface TerminalTarget {
  session: SessionId;
  shell: string;
}

/** Where the stream is. */
export type LinkStatus =
  | { state: 'connecting' }
  /** Attached: output flows and typing goes through. */
  | { state: 'live' }
  /** The server can't reach the shell's machine; output resumes when it's back. */
  | { state: 'detached'; reason: string }
  /** The socket dropped; reconnecting (with a fresh replay). */
  | { state: 'offline' }
  /** The shell's process ended (or the shell is gone); reconnect() tries again. */
  | { state: 'ended'; reason: string }
  /** The login is gone; reconnect() after signing in. */
  | { state: 'signed_out' };

export interface TerminalHandlers {
  /** Clear the terminal: a fresh replay of recent output follows. */
  reset(): void;
  /** Terminal output (may split escape sequences and UTF-8 characters). */
  output(data: Uint8Array): void;
  /** The terminal's foreground program; null at the shell's prompt. */
  foreground(name: string | null): void;
  status(status: LinkStatus): void;
}

export interface TerminalLink {
  /** Types into the terminal (dropped unless live). */
  input(data: string | Uint8Array): void;
  /** The terminal's size; sent again after every reconnect. */
  resize(cols: number, rows: number): void;
  /** Attaches again after `ended` or `signed_out`. */
  reconnect(): void;
  close(): void;
}
