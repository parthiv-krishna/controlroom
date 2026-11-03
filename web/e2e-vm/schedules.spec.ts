import { expect, test } from '@playwright/test';
import { header, login, sessionRow, timeline } from './helpers';

// A schedule on the session: added in the Schedules panel, "Run now" sends its message to pi
// through the server's scheduler (the mock model answers), and Settings › Schedules lists it.
// It is disabled from the overview at the end, so it never fires during later scenarios.
test('a schedule runs now and shows in the overview', async ({ page }) => {
  await login(page, 'Laptop');
  await sessionRow(page, 'hello').click();
  await header(page).getByRole('button', { name: 'Schedules' }).click();
  const panel = page.locator('[data-pane="schedules"]');
  await panel.getByRole('button', { name: '+ Add schedule' }).click();
  await panel.getByLabel('Schedule name').fill('Inbox');
  await panel.getByLabel('Cron expression').fill('0 9 * * mon-fri');
  await panel.getByLabel('Time zone').fill('Europe/Berlin');
  await panel.getByLabel('Schedule message').fill('Check inbox/ and report');
  await panel.getByRole('button', { name: 'Save' }).click();
  const row = panel.locator('[data-schedule]').filter({ hasText: 'Inbox' });
  await expect(row).toContainText('Weekdays 09:00 · Europe/Berlin');
  await expect(row).toContainText(/next \w{3} 09:00/);

  await row.getByRole('button', { name: 'Run now' }).click();
  await expect(row.getByRole('button', { name: 'fired' })).toBeVisible();
  const tl = timeline(page);
  await expect(tl.locator('.msg.r-user').last()).toHaveText('Check inbox/ and report');
  await expect(tl.locator('.msg.r-agent').last()).toHaveText('Inbox is empty.');
  // The run is badged with the schedule (the daemon maps the prompt's origin to its trigger).
  await expect(tl.locator('.trig.t-schedule').last()).toHaveText('schedule: Inbox');

  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Schedules' }).click();
  const listed = settings.locator('[data-schedule]').filter({ hasText: 'Inbox' });
  await expect(listed).toContainText('hello');
  await expect(listed).toContainText('last fired');
  await listed.getByLabel('Enable Inbox').uncheck();
  await expect(listed).toHaveClass(/\boff\b/);
  await page.getByRole('button', { name: 'Close settings' }).click();
  await expect(row).toHaveClass(/\boff\b/);
  await expect(row).toContainText('disabled');
});
