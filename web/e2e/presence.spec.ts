import { expect, type Page, test } from '@playwright/test';
import { calls, start } from './helpers';

// Presence: the tab reports user input as a throttled heartbeat (client.activity) while it is
// shown, and its visibility with client.focus; the server holds pushes meanwhile.

/** Makes the page look hidden (or visible again) and fires visibilitychange. */
async function setHidden(page: Page, hidden: boolean): Promise<void> {
  await page.evaluate((hidden) => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => (hidden ? 'hidden' : 'visible') });
    document.dispatchEvent(new Event('visibilitychange'));
  }, hidden);
}

const activity = async (page: Page) => (await calls(page, 'client.activity')).length;
const lastFocus = async (page: Page) => (await calls(page, 'client.focus')).at(-1)?.params;

test('input is reported once per interval while shown; hiding reports not visible', async ({ page }) => {
  await start(page);
  await page.mouse.move(10, 10);
  await expect.poll(() => activity(page)).toBe(1);
  for (let x = 20; x < 200; x += 20) await page.mouse.move(x, 40);
  await page.mouse.down();
  await page.mouse.up();
  expect(await activity(page)).toBe(1);

  await setHidden(page, true);
  await expect.poll(() => lastFocus(page)).toMatchObject({ visible: false });
  await page.mouse.move(300, 300);
  expect(await activity(page)).toBe(1);

  // Shown again: the next input reports at once.
  await setHidden(page, false);
  await expect.poll(() => lastFocus(page)).toMatchObject({ visible: true });
  await page.mouse.move(310, 310);
  await expect.poll(() => activity(page)).toBe(2);
});

test('Settings › Notifications: delay, hold while active and the idle window', async ({ page }) => {
  await start(page);
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Notifications' }).click();
  await expect(settings.getByRole('button', { name: 'Push delay' })).toHaveText(/30 s/);
  await expect(settings.getByRole('checkbox', { name: "Hold while I'm active" })).toBeChecked();
  await expect(settings.getByRole('button', { name: 'Idle after' })).toHaveText(/2 min/);

  await settings.getByRole('button', { name: 'Push delay' }).click();
  await expect(settings.getByRole('listbox', { name: 'Push delay' }).getByRole('option')).toHaveText(['None', '30 s', '1 min', '2 min', '5 min']);
  await settings.getByRole('listbox', { name: 'Push delay' }).getByRole('option', { name: '1 min' }).click();
  await settings.getByRole('button', { name: 'Idle after' }).click();
  await settings.getByRole('listbox', { name: 'Idle after' }).getByRole('option', { name: '5 min' }).click();
  await settings.getByRole('checkbox', { name: "Hold while I'm active" }).uncheck();
  await expect
    .poll(async () => (await calls(page, 'notifications.set_preferences')).map((c) => c.params))
    .toEqual([{ delay_s: 60 }, { idle_s: 300 }, { hold_while_active: false }]);
});
