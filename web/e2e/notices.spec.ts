import { expect, test, type Page } from '@playwright/test';
import fixture from '../fixtures/controlroom.json' with { type: 'json' };
import timelines from '../fixtures/timelines.json' with { type: 'json' };
import type { FeedSession, TimelineItem } from '../src/lib/protocol';
import { S, calls, emitSession, emitState, start } from './helpers';

// The agent's warning and error notices (`notify_user`) stay pinned above the composer until the
// user dismisses them; info notices only show in the timeline.
const tray = (page: Page) => page.getByRole('main').getByRole('region', { name: 'Requests' });
const timeline = (page: Page) => page.getByRole('main').getByLabel('Timeline');
// SAFETY: JSON imports widen string unions; fixture.test.ts checks the fixture's consistency.
const inbox = fixture.snapshot.sessions.find((s) => s.id === S.inbox) as unknown as FeedSession;

/** What the server sends once the user dismissed `itemId` (the summary and the item). */
async function dismissed(page: Page, itemId: string) {
  const notices = inbox.notices.filter((n) => n.item_id !== itemId);
  await emitState(page, [{ kind: 'session_upserted', session: { ...inbox, notices, attention: notices.length ? 'notice' : null } }]);
  // SAFETY: as above.
  const recorded = timelines as unknown as Record<string, { items: TimelineItem[] }>;
  const open = recorded[S.inbox].items.find((i) => i.id === itemId)!;
  const item = { ...open, dismissed: { by: { device_id: 'device-laptop', label: 'Laptop (Firefox on Linux)' }, at: open.ts + 1000 } } as TimelineItem;
  await emitSession(page, S.inbox, [{ op: 'upsert', item }]);
}

test('agent notices: pinned with Dismiss, a stub in the timeline; info and dismissed ones are not pinned', async ({ page }) => {
  await start(page, S.inbox);
  const pinned = tray(page).locator('[data-pinned]');
  await expect(pinned).toHaveCount(2);
  await expect(tray(page)).toContainText('2 notices');
  // Oldest first: the warning, then the error.
  await expect(pinned.first()).toContainText('devbox has a failed unit');
  await expect(pinned.nth(1).getByRole('img', { name: 'Error' })).toBeVisible();
  // The timeline keeps a one-line stub that jumps to the tray; info and the dismissed one show in full.
  const stub = timeline(page).locator('[data-stub="an.warning"]');
  await expect(stub).toContainText('devbox has a failed unit');
  await expect(timeline(page).getByText('Checked 4 units on devbox.')).toBeVisible();
  await expect(tray(page)).not.toContainText('Checked 4 units');
  await expect(timeline(page).locator('[title="Dismissed on Phone"]')).toContainText('devbox rebooted overnight');
  await stub.click();
  await expect(pinned.first().getByRole('button', { name: 'Dismiss warning notice' })).toBeFocused();
  // The session list shows the notice state.
  await expect(page.locator(`[data-session="${S.inbox}"] [title^="Agent notice"]`)).toBeVisible();

  await pinned.first().getByRole('button', { name: 'Dismiss warning notice' }).click();
  await expect.poll(async () => (await calls(page, 'notice.dismiss')).at(-1)?.params).toMatchObject({ id: S.inbox, item_id: 'an.warning' });
  await dismissed(page, 'an.warning');
  await expect(pinned).toHaveCount(1);
  await expect(tray(page)).not.toContainText('notices');
  await expect(timeline(page).locator('[data-stub="an.warning"]')).toHaveCount(0);
  await expect(timeline(page).locator('[title="Dismissed on Laptop (Firefox on Linux)"]')).toContainText('devbox has a failed unit');
});

test('agent notices: dismissed from the notifications panel, gone from the tray live', async ({ page }) => {
  await start(page, S.inbox);
  await page.locator('body').press(']');
  const panel = page.getByRole('complementary', { name: 'Notifications' });
  await expect(panel.getByText('Agent notices')).toBeVisible();
  const cards = panel.locator('[data-notice]');
  await expect(cards).toHaveCount(2);
  // Newest first in the panel.
  await expect(cards.first()).toContainText('disk on devbox');
  await cards.first().getByRole('button', { name: 'Dismiss error notice' }).click();
  await expect.poll(async () => (await calls(page, 'notice.dismiss')).at(-1)?.params).toMatchObject({ id: S.inbox, item_id: 'an.error' });
  await dismissed(page, 'an.error');
  await expect(cards).toHaveCount(1);
  await expect(tray(page).locator('[data-pinned]')).toHaveCount(1);
  await expect(tray(page)).not.toContainText('disk on devbox');
});

test('agent notices: on a phone the tray keeps the composer on screen', async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 390, height: 700 }, hasTouch: true, isMobile: true });
  await page.goto(`/?paused#/s/${S.inbox}`);
  await expect(tray(page).locator('[data-pinned]')).toHaveCount(2);
  await expect(page.getByLabel('Message')).toBeInViewport();
  const dismiss = tray(page).getByRole('button', { name: 'Dismiss warning notice' });
  await expect(dismiss).toBeInViewport();
  await dismiss.tap();
  await expect.poll(async () => (await calls(page, 'notice.dismiss')).length).toBe(1);
  await page.close();
});

test('Settings › Notifications: which agent notices push', async ({ page }) => {
  await start(page);
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Notifications' }).click();
  const picker = settings.getByRole('button', { name: 'Push agent notices' });
  await expect(picker).toHaveText(/Warning and error/);
  await picker.click();
  const options = settings.getByRole('listbox', { name: 'Push agent notices' }).getByRole('option');
  await expect(options).toHaveText(['Warning and error', 'Errors only', 'Off']);
  await options.filter({ hasText: 'Errors only' }).click();
  await expect.poll(async () => (await calls(page, 'notifications.set_preferences')).map((c) => c.params)).toEqual([{ agent_notices: 'error' }]);
});
