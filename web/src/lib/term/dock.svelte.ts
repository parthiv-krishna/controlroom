// The terminal dock's view state for this visit, per session: whether it is open, which shell's
// tab is selected and its height. `focusShell` opens a shell's tab and focuses its terminal (the
// `terminal_input` request card uses it).
import type { SessionId } from '../protocol';

export interface DockState {
  open: boolean;
  /** The selected shell; null: the first one. */
  shell: string | null;
  /** The desktop dock's height in pixels. */
  height: number;
}

export const DOCK_MIN_HEIGHT = 140;
const DEFAULT_HEIGHT = 280;

class Docks {
  private states = $state<Record<SessionId, DockState>>({});
  /** A request for the open terminal to take the keyboard focus. */
  focusRequest = $state<{ session: SessionId; shell: string; n: number } | null>(null);

  get(session: SessionId): DockState {
    return this.states[session] ?? { open: false, shell: null, height: DEFAULT_HEIGHT };
  }

  update(session: SessionId, patch: Partial<DockState>): void {
    this.states[session] = { ...this.get(session), ...patch };
  }

  /** Opens the dock at `shell` and focuses its terminal. */
  focusShell(session: SessionId, shell: string): void {
    this.update(session, { open: true, shell });
    this.focusRequest = { session, shell, n: (this.focusRequest?.n ?? 0) + 1 };
  }
}

export const docks = new Docks();
