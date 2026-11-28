import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TOAST_MS, Toasts } from './overlays.svelte';

describe('Toasts', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('collapses identical toasts while one shows, and keeps it up', () => {
    const toasts = new Toasts();
    toasts.show('Busy, try again', 'error');
    vi.advanceTimersByTime(TOAST_MS - 100);
    toasts.show('Busy, try again', 'error');
    toasts.show('Busy, try again', 'info');
    toasts.show('Saved', 'ok');
    expect(toasts.list.map((t) => [t.text, t.level])).toEqual([
      ['Busy, try again', 'error'],
      ['Busy, try again', 'info'],
      ['Saved', 'ok'],
    ]);
    vi.advanceTimersByTime(200);
    expect(toasts.list.map((t) => t.text)).toContain('Busy, try again');
    vi.advanceTimersByTime(TOAST_MS);
    expect(toasts.list).toEqual([]);
    // Once gone, the same text shows again.
    toasts.show('Busy, try again', 'error');
    expect(toasts.list).toHaveLength(1);
  });
});
