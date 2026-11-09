import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { choose, closeSettings, env, header, lastReply, login, newSession, settled } from './helpers';

// Settings › Models on the real server: the server's `mock` provider (Nix: route, models.json and a
// systemd credential) shows read-only; a custom gateway `mock-ui` to the same mock model is added
// here with its key, tested, and made the default, and a new session answers through it (its model
// shows as the gateway's, named by its id where the server's is named "Mock"). The defaults go back
// to the server's at the end, so later scenarios keep using the Nix-declared provider.
test('a gateway added in Settings › Models runs a session', async ({ page }) => {
  const key = readFileSync(env('E2E_MODEL_KEY_FILE'), 'utf8').trim();
  await login(page, 'Laptop');
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Models' }).click();
  await expect(settings.locator('[data-provider="mock"] [data-status]')).toHaveText('from server config');

  await settings.getByRole('button', { name: '+ Gateway' }).click();
  const form = settings.getByRole('group', { name: 'New gateway' });
  await form.getByLabel('Gateway name').fill('mock-ui');
  await form.getByLabel('Gateway base URL').fill('http://127.0.0.1:4000/v1');
  await form.getByLabel('Gateway models').fill('mock-model');
  await form.getByLabel('Gateway API key').fill(key);
  await form.getByRole('button', { name: 'Save gateway' }).click();
  const row = settings.locator('[data-provider="mock-ui"]');
  await expect(row.locator('[data-status]')).toHaveText(`key set …${key.slice(-4)}`);
  await expect(row).toContainText('by Laptop');
  await row.getByRole('button', { name: 'Test mock-ui' }).click();
  await expect(row.locator('[data-status]')).toHaveText('ok');
  await expect(row).toContainText('HTTP 200 OK');

  const defaults = settings.locator('[data-defaults]');
  await choose(defaults, 'Default provider', 'mock-ui');
  await choose(defaults, 'Default model', 'mock-model');
  await closeSettings(page);

  await newSession(page, { workspaces: [{ machine: 'devbox', path: '~/work/models' }], name: 'ui-key', level: 'Strict', prompt: 'Which key?' });
  await expect(lastReply(page)).toHaveText('Answered through the UI key.');
  await settled(page);
  await expect(header(page).getByRole('button', { name: 'Model', exact: true })).toHaveText(/mock-model/);

  await page.getByRole('button', { name: 'Settings' }).click();
  await settings.getByRole('tab', { name: 'Models' }).click();
  await choose(defaults, 'Default provider', /^Server \(mock/);
  await expect(defaults.getByRole('button', { name: 'Default provider', exact: true })).toContainText('Server (mock/mock-model)');
  await closeSettings(page);
});
