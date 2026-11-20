import { expect, test } from '@playwright/test';
import { sessionAction, login, openSession, timeline } from './helpers';

// A schedule on the session: added in the Schedules panel, "Run now" sends its message to pi
// through the server's scheduler and the agent daemon (the mock model answers), and the timeline
// shows it as an event row. It is disabled at the end, so it never fires during later scenarios.
test('a schedule runs now, shows as an event, and is disabled from its row', async ({ page }) => {
  await login(page, 'Laptop');
  await openSession(page, 'two');
  await sessionAction(page, /^Schedules/);
  const panel = page.locator('[data-pane="schedules"]');
  await panel.getByRole('button', { name: '+ Schedule' }).click();
  await panel.getByLabel('Schedule name').fill('Inbox');
  await panel.getByLabel('Cron expression').fill('0 9 * * mon-fri');
  await panel.getByLabel('Time zone').fill('Europe/Berlin');
  await panel.getByLabel('Schedule message').fill('Check inbox/ and report');
  await panel.getByRole('button', { name: 'Save' }).click();
  const row = panel.locator('[data-schedule]').filter({ hasText: 'Inbox' });
  await expect(row.locator('[data-when]')).toHaveText('Weekdays 09:00');
  await expect(row.locator('[data-when]')).toHaveAttribute('title', 'Time zone: Europe/Berlin');
  await expect(row).toContainText(/next \w{3} 09:00/);

  await row.getByRole('button', { name: 'Run now' }).click();
  await expect(row.getByRole('button', { name: 'fired' })).toBeVisible();
  const tl = timeline(page);
  await expect(tl.locator('.msg.r-agent').last()).toHaveText('Inbox is empty.');
  // The prompt is an event row naming the schedule; what the agent was told opens on click.
  const event = tl.locator('[data-event="schedule"]').last();
  await expect(event.getByRole('button')).toHaveText('◷Schedule Inbox');
  await event.getByRole('button').click();
  await expect(event.locator('.text')).toHaveText('Check inbox/ and report');

  // Disabled from its row (it never fires during later scenarios).
  await row.getByLabel('Enable Inbox').uncheck();
  await expect(row).toHaveClass(/\boff\b/);
  await expect(row).toContainText('off');
});
