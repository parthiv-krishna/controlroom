// Svelte actions shared by menus and popovers.
import type { Action } from 'svelte/action';

export interface Dismiss {
  /** Closes the popup. */
  close: () => void;
  /** The toggle that opened the popup: a mousedown there is not "outside" (it toggles). */
  anchor?: HTMLElement | null;
}

/**
 * For a popup that is mounted only while open: calls `close` on a mousedown outside it (and its
 * anchor), or on Escape, which it consumes.
 */
export const dismissable: Action<HTMLElement, Dismiss> = (node, initial) => {
  let opts = initial;
  const down = (e: MouseEvent) => {
    const t = e.target as Node;
    if (!node.contains(t) && !opts.anchor?.contains(t)) opts.close();
  };
  const key = (e: KeyboardEvent) => {
    if (e.key !== 'Escape') return;
    e.stopPropagation();
    e.preventDefault();
    opts.close();
  };
  window.addEventListener('mousedown', down);
  window.addEventListener('keydown', key, true);
  return {
    update(next: Dismiss) {
      opts = next;
    },
    destroy() {
      window.removeEventListener('mousedown', down);
      window.removeEventListener('keydown', key, true);
    },
  };
};

/** Focuses the node when mounted (and puts the caret at the end of a text field). */
export const autofocus: Action<HTMLElement> = (node) => {
  queueMicrotask(() => {
    node.focus();
    if (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement) node.setSelectionRange(node.value.length, node.value.length);
  });
};
