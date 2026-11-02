import { expect, test } from '@playwright/test';
import { env, login, machineCard, openMachines, step } from './helpers';

// Add a machine through Settings › Machines: confirm its host key, install the controlroom key
// (after "permission denied", or up front when E2E_KEY_FIRST is set), then the server installs
// the daemon and shows the doctor results.
//   E2E_HOST: the host (also its display name); E2E_HOST_KEY: its ed25519 fingerprint.
test('add a machine', async ({ page }) => {
  const host = env('E2E_HOST');
  const keyFirst = env('E2E_KEY_FIRST') !== '';
  await login(page, 'Laptop');
  const settings = await openMachines(page);
  await settings.getByRole('button', { name: '+ Add machine' }).click();
  await settings.getByLabel('Host').fill(host);
  await settings.getByLabel('User').fill('alice');
  await settings.getByLabel('Display name').fill(host);

  if (keyFirst) {
    // The form shows the key's install command; the user runs it on the host first.
    const command = settings.locator('.ib code');
    await expect(command).toContainText('authorized_keys');
    await step('install-key', { command: await command.innerText() });
  }
  await settings.getByRole('button', { name: 'Connect' }).click();

  await expect(settings.getByRole('heading', { name: 'Confirm host key' })).toBeVisible();
  await expect(settings.locator('pre.fp')).toHaveText(`ssh-ed25519  ${env('E2E_HOST_KEY')}`);
  await settings.getByRole('button', { name: 'Trust and continue' }).click();

  if (!keyFirst) {
    await expect(settings.getByRole('heading', { name: 'Permission denied' })).toBeVisible();
    const command = settings.locator('.ib code');
    await expect(command).toContainText('authorized_keys');
    await step('install-key', { command: await command.innerText() });
    await settings.getByRole('button', { name: 'Try again' }).click();
  }

  await expect(settings.getByRole('heading', { name: `Setting up ${host}` })).toBeVisible();
  await expect(settings.getByText(/installing controlroomd/)).toBeVisible();
  const doctor = settings.locator('.doctor');
  await expect(doctor.locator('.r', { hasText: 'Sandbox (nono)' })).toHaveClass(/\bok\b/, { timeout: 120_000 });
  await expect(doctor.locator('.r', { hasText: 'Landlock' })).toContainText(/ABI \d+/);
  await expect(doctor.locator('.r', { hasText: /^✓pi/ })).toHaveClass(/\bok\b/);
  await settings.getByRole('button', { name: 'Done' }).click();

  const card = machineCard(settings, host);
  await expect(card.locator('.chip')).toHaveText('connected');
  await expect(card).toContainText(`alice@${host}:22`);
  await expect(card).toContainText(/controlroomd \S+ · x86_64/);
});
