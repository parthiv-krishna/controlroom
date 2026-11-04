// Keys and terminals (spec §13): Ctrl-h/j/k/l move pane focus, except in a terminal whose
// foreground program is on the pass-through list (then the keys go to the PTY, like
// vim-tmux-navigator); Ctrl-\ followed by a key always sends that key to the terminal. On the
// phone an extra-keys row stands in for the keys a touch keyboard lacks.

/** Programs that take Ctrl-h/j/k/l themselves. */
export const PASSTHROUGH: readonly string[] = ['vim', 'nvim', 'vi', 'fzf', 'less', 'man', 'htop', 'lazygit'];

/** Whether the program in a terminal's foreground takes the navigator keys. */
export const passesKeys = (foreground: string | null): boolean => !!foreground && PASSTHROUGH.includes(foreground);

/** Set on a terminal's element while it takes the navigator keys (pass-through or after Ctrl-\). */
export const PASS_ATTR = 'data-term-pass';

/** Whether the focused element is in a terminal that takes the navigator keys. */
export const terminalTakesKeys = (focused: Element | null): boolean => !!focused?.closest(`[${PASS_ATTR}]`);

/** The escape-hatch prefix: Ctrl-\. */
export const isPrefix = (e: KeyboardEvent): boolean => e.ctrlKey && !e.altKey && !e.metaKey && e.key === '\\';

/** The phone's extra keys: a label and the bytes it types; `Ctrl` is a one-shot modifier. */
export const EXTRA_KEYS: readonly { label: string; name: string; seq: string }[] = [
  { label: 'Esc', name: 'Escape', seq: '\x1b' },
  { label: 'Tab', name: 'Tab', seq: '\t' },
  { label: 'Ctrl', name: 'Control', seq: '' },
  { label: '←', name: 'Left', seq: '\x1b[D' },
  { label: '↑', name: 'Up', seq: '\x1b[A' },
  { label: '↓', name: 'Down', seq: '\x1b[B' },
  { label: '→', name: 'Right', seq: '\x1b[C' },
  { label: '|', name: 'Pipe', seq: '|' },
  { label: '~', name: 'Tilde', seq: '~' },
  { label: '/', name: 'Slash', seq: '/' },
];

/** The control character for a typed character (`c` → ETX), or null if it has none. */
export function ctrlChar(ch: string): string | null {
  if (ch.length !== 1) return null;
  const code = ch.toUpperCase().charCodeAt(0);
  // @ A–Z [ \ ] ^ _ map to 0x00–0x1f.
  return code >= 0x40 && code <= 0x5f ? String.fromCharCode(code - 0x40) : null;
}
