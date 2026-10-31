// Svelte actions shared by menus and popovers.
import type { Action } from 'svelte/action';

/** Calls `close` on a mousedown outside the node or on Escape (which it consumes). */
export const dismissable: Action<HTMLElement, () => void> = (node, close) => {
  let handler = close;
  const down = (e: MouseEvent) => {
    if (!node.contains(e.target as Node)) handler();
  };
  const key = (e: KeyboardEvent) => {
    if (e.key !== 'Escape') return;
    e.stopPropagation();
    e.preventDefault();
    handler();
  };
  window.addEventListener('mousedown', down);
  window.addEventListener('keydown', key, true);
  return {
    update(next: () => void) {
      handler = next;
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
