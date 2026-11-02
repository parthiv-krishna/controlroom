import { expect, test } from '@playwright/test';
import { closeSettings, header, login, machineCard, openMachines, send, sessionRow, step, timeline } from './helpers';

// The test script unplugs buildbox's network. The SSH keepalives notice, the machine shows as
// unreachable, and its session keeps its last known (stale) state, read-only. devbox keeps
// working meanwhile. Once the network is back it reconnects (the server retries with a backoff;
// "Retry now" skips the wait).
test('an unreachable machine shows its stale state and does not block the other', async ({ page }) => {
  await login(page, 'Laptop');
  await step('unplug');

  const settings = await openMachines(page);
  await expect(machineCard(settings, 'buildbox').locator('.chip')).toHaveText('unreachable', { timeout: 180_000 });
  await expect(machineCard(settings, 'devbox').locator('.chip')).toHaveText('connected');
  await closeSettings(page);

  await sessionRow(page, 'build').click();
  const banner = page.locator('.banner', { hasText: 'buildbox unreachable' });
  await expect(banner).toContainText('last seen');
  await expect(header(page).locator('.status')).toHaveText('Disconnected');
  await expect(timeline(page).locator('.msg.r-agent').last()).toHaveText('Still here.');
  await expect(page.getByLabel('Message')).toBeDisabled();
  await expect(page.getByLabel('Message')).toHaveAttribute('placeholder', 'Machine offline');

  // devbox is unaffected.
  await sessionRow(page, 'hello').click();
  await send(page, 'Still there?');
  await expect(timeline(page).locator('.msg.r-agent').last()).toHaveText('Devbox still works.');

  await step('plug');
  await sessionRow(page, 'build').click();
  const retry = banner.getByRole('button', { name: 'Retry now' });
  await expect(async () => {
    if (await retry.isVisible()) await retry.click({ timeout: 1_000 });
    await expect(page.locator('.banner')).toHaveCount(0, { timeout: 5_000 });
  }).toPass({ timeout: 180_000 });
  await expect(page.getByLabel('Message')).toBeEnabled();
  await expect(header(page).locator('.status')).toHaveText('Complete');
});
