// Pane navigator (spec §13): Ctrl-h/j/k/l moves focus spatially between the visible panes, from
// anywhere (text fields included), like vim-tmux-navigator. Panes mark themselves `data-pane`.

interface Pane {
  name: string;
  el: HTMLElement;
}

const isVisible = (el: HTMLElement): boolean => {
  const r = el.getBoundingClientRect();
  return r.width > 4 && r.height > 4 && getComputedStyle(el).visibility !== 'hidden';
};

function panes(): Pane[] {
  return [...document.querySelectorAll<HTMLElement>('[data-pane]')].filter(isVisible).map((el) => ({ name: el.dataset.pane ?? '', el }));
}

/** The innermost pane containing `node`. */
function paneOf(node: Element | null): Pane | null {
  let best: Pane | null = null;
  for (const p of panes()) if (node && p.el.contains(node) && (!best || best.el.contains(p.el))) best = p;
  return best;
}

const remembered = new WeakMap<HTMLElement, HTMLElement>();

function focusPane(p: Pane): void {
  const last = remembered.get(p.el);
  const target =
    (last?.isConnected && p.el.contains(last) ? last : null) ??
    p.el.querySelector<HTMLElement>('[aria-selected="true"], textarea, input, button, [tabindex]');
  if (!p.el.hasAttribute('tabindex')) p.el.tabIndex = -1;
  (target ?? p.el).focus();
}

/** Moves focus to the nearest pane in a direction; false when there is none. */
export function moveFocus(dir: 'h' | 'j' | 'k' | 'l'): boolean {
  const all = panes();
  const cur = paneOf(document.activeElement) ?? all.find((p) => p.name === 'session') ?? null;
  if (!cur) return false;
  const a = cur.el.getBoundingClientRect();
  const cx = a.left + a.width / 2;
  const cy = a.top + a.height / 2;
  let best: { p: Pane; d: number } | null = null;
  for (const p of all) {
    if (p.el === cur.el || p.el.contains(cur.el) || cur.el.contains(p.el)) continue;
    const r = p.el.getBoundingClientRect();
    const ahead = dir === 'h' ? r.right <= a.left + 8 : dir === 'l' ? r.left >= a.right - 8 : dir === 'k' ? r.bottom <= a.top + 8 : r.top >= a.bottom - 8;
    if (!ahead) continue;
    const px = Math.max(r.left, Math.min(cx, r.right));
    const py = Math.max(r.top, Math.min(cy, r.bottom));
    const horizontal = dir === 'h' || dir === 'l';
    const d = (horizontal ? Math.abs(px - cx) : Math.abs(py - cy)) + 2 * (horizontal ? Math.abs(py - cy) : Math.abs(px - cx));
    if (!best || d < best.d) best = { p, d };
  }
  if (!best) return false;
  focusPane(best.p);
  return true;
}

/** Rings the focused pane and remembers its last focused element. */
export function trackPaneFocus(): () => void {
  let ringed: HTMLElement | null = null;
  const onFocus = () => {
    const p = paneOf(document.activeElement);
    if (ringed && ringed !== p?.el) ringed.classList.remove('pane-focus');
    if (!p) return;
    p.el.classList.add('pane-focus');
    ringed = p.el;
    if (document.activeElement instanceof HTMLElement) remembered.set(p.el, document.activeElement);
  };
  document.addEventListener('focusin', onFocus);
  return () => document.removeEventListener('focusin', onFocus);
}
