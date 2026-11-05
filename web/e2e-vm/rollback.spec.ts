import { expect, test } from '@playwright/test';
import { closeSettings, env, login, machineCard, openMachines, openSession, send, timeline } from './helpers';

// The server now bundles a "newer" daemon that can't serve. The automatic upgrades fail and are
// rolled back: the machines stay connected on their daemon, say what went wrong, and still run.
// Turning auto-upgrade off stops the retries.
test('a failed automatic upgrade is rolled back', async ({ page }) => {
  const version = env('E2E_VERSION');
  await login(page, 'Laptop');
  const settings = await openMachines(page);
  for (const name of ['devbox', 'buildbox']) {
    const card = machineCard(settings, name);
    await expect(card.getByRole('status')).toContainText('automatic upgrade failed', { timeout: 120_000 });
    await expect(card.getByRole('status')).toContainText(`rolled back to versions/${version}`);
    await expect(card.locator('.chip')).toHaveText('connected');
    await expect(card).toContainText(`controlroomd ${version} ·`);
    await expect(card).toContainText('update available');
    await expect(card.getByRole('button', { name: /^Upgrade the daemon/ })).toBeVisible();
    await card.getByLabel('Auto-upgrade').uncheck();
    await expect(card.getByLabel('Auto-upgrade')).not.toBeChecked();
  }
  await closeSettings(page);

  await openSession(page, 'build');
  await send(page, 'Still working?');
  await expect(timeline(page).locator('.msg.r-agent').last()).toHaveText('Rolled back fine.');
});
