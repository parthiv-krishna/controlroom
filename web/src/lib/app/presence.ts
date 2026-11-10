// Presence: the server holds pushes while the user is at one of their devices. This tab tells it
// whether it is shown (`client.focus`, visible and focused) and, while shown, that the user is
// using it: a throttled heartbeat on key, pointer and touch input (`client.activity`). Hidden,
// blurred or closed tabs stop counting at once; an idle tab stops after the server's idle window.

/** The shortest time between two input reports; well below the server's idle window (≥ 1 min). */
export const ACTIVITY_INTERVAL_MS = 20_000;

/** The input that shows the user is at this device. */
const INPUT_EVENTS = ['keydown', 'pointerdown', 'pointermove', 'wheel', 'touchstart'] as const;
/** What changes whether the tab is shown. */
const SHOWN_EVENTS = ['focus', 'blur'] as const;

/** Whether the page is visible and focused. */
export function pageShown(doc: Document = document): boolean {
  return doc.visibilityState === 'visible' && doc.hasFocus();
}

/** Reports input at most once per `interval` while the page is shown. */
export class InputHeartbeat {
  private last: number | null = null;

  constructor(
    private readonly report: () => void,
    private readonly shown: () => boolean = () => pageShown(),
    private readonly now: () => number = Date.now,
    private readonly interval = ACTIVITY_INTERVAL_MS,
  ) {}

  /** The user used the page. */
  input(): void {
    if (!this.shown()) return;
    const now = this.now();
    if (this.last !== null && now - this.last < this.interval) return;
    this.last = now;
    this.report();
  }

  /** The next input reports at once: the page was shown again or the connection reopened. */
  reset(): void {
    this.last = null;
  }
}

/** Calls `input` on user input and `changed` when the page is shown or hidden; returns the cleanup. */
export function watchPresence(win: Window, on: { input: () => void; changed: () => void }): () => void {
  const options = { capture: true, passive: true };
  for (const name of INPUT_EVENTS) win.addEventListener(name, on.input, options);
  for (const name of SHOWN_EVENTS) win.addEventListener(name, on.changed);
  win.document.addEventListener('visibilitychange', on.changed);
  return () => {
    for (const name of INPUT_EVENTS) win.removeEventListener(name, on.input, options);
    for (const name of SHOWN_EVENTS) win.removeEventListener(name, on.changed);
    win.document.removeEventListener('visibilitychange', on.changed);
  };
}
