import { describe, expect, it } from 'vitest';
import { ACTIVITY_INTERVAL_MS, InputHeartbeat, pageShown, watchPresence } from './presence';

/** A heartbeat on a manual clock, with the page shown unless `shown.value` is false. */
function heartbeat() {
  const clock = { now: 0 };
  const shown = { value: true };
  const reports: number[] = [];
  const beat = new InputHeartbeat(() => reports.push(clock.now), () => shown.value, () => clock.now);
  return { beat, clock, shown, reports };
}

describe('InputHeartbeat', () => {
  it('reports input at most once per interval', () => {
    const { beat, clock, reports } = heartbeat();
    beat.input();
    for (clock.now = 1000; clock.now < ACTIVITY_INTERVAL_MS; clock.now += 1000) beat.input();
    expect(reports).toEqual([0]);
    clock.now = ACTIVITY_INTERVAL_MS;
    beat.input();
    beat.input();
    expect(reports).toEqual([0, ACTIVITY_INTERVAL_MS]);
  });

  it('reports nothing while the page is hidden or blurred, and at once when it is back', () => {
    const { beat, clock, shown, reports } = heartbeat();
    beat.input();
    shown.value = false;
    clock.now = 60_000;
    beat.input();
    expect(reports).toEqual([0]);
    // Shown again (the app resets the heartbeat with its focus report): the next input counts.
    shown.value = true;
    clock.now = 61_000;
    beat.reset();
    beat.input();
    clock.now = 62_000;
    beat.input();
    expect(reports).toEqual([0, 61_000]);
  });
});

describe('watchPresence', () => {
  it('routes input and visibility changes until stopped', () => {
    const seen: string[] = [];
    const stop = watchPresence(window, { input: () => seen.push('input'), changed: () => seen.push('changed') });
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'a' }));
    window.dispatchEvent(new Event('pointerdown'));
    window.dispatchEvent(new Event('touchstart'));
    window.dispatchEvent(new Event('blur'));
    window.dispatchEvent(new Event('focus'));
    document.dispatchEvent(new Event('visibilitychange'));
    expect(seen).toEqual(['input', 'input', 'input', 'changed', 'changed', 'changed']);
    stop();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'a' }));
    window.dispatchEvent(new Event('blur'));
    expect(seen).toHaveLength(6);
  });

  it('counts a page as shown only while visible and focused', () => {
    const doc = (visibilityState: DocumentVisibilityState, focused: boolean) =>
      ({ visibilityState, hasFocus: () => focused }) as Document;
    expect(pageShown(doc('visible', true))).toBe(true);
    expect(pageShown(doc('visible', false))).toBe(false);
    expect(pageShown(doc('hidden', true))).toBe(false);
  });
});
