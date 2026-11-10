import { expect, test, type Page } from '@playwright/test';
import fixture from '../fixtures/controlroom.json' with { type: 'json' };
import type { LoginState, ModelsState } from '../src/lib/protocol';
import { calls, start } from './helpers';

/** Opens Settings › Models (on a phone, through the projects drawer). */
async function openModels(page: Page, phone = false) {
  if (phone) await page.getByRole('button', { name: 'Projects' }).click();
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Models' }).click();
  await expect(settings.getByRole('tab', { name: 'Models' })).toHaveAttribute('aria-selected', 'true');
  return settings;
}

const login = (seq: number, step: LoginState['step']): LoginState => ({ login_id: '01JAL000000000000000000001', provider: 'anthropic', seq, step });

test('models: statuses, add, replace and remove a key, test', async ({ page }) => {
  await start(page);
  const settings = await openModels(page);
  const row = (id: string) => settings.locator(`[data-provider="${id}"]`);
  const status = (id: string) => row(id).locator('[data-status]');
  await expect(status('anthropic')).toHaveText('not set up');
  await expect(status('openai')).toHaveText('key set …4f2a');
  await expect(row('openai')).toContainText('Laptop (Firefox on Linux)');
  await expect(status('openrouter')).toHaveText('ok');
  await expect(status('openai-codex')).toHaveText('signed in');
  await expect(row('openai-codex')).toContainText('alice@example.com');
  await expect(status('google')).toHaveText('test failed');
  await expect(row('google')).toContainText('HTTP 403 Forbidden');
  await expect(status('litellm')).toHaveText('from server config');
  await expect(row('litellm').getByRole('button', { name: /Remove|key/ })).toHaveCount(0);

  // Add: the key goes out once, and the form closes.
  await row('anthropic').getByRole('button', { name: 'Add API key or subscription token for Anthropic' }).click();
  const input = row('anthropic').getByRole('textbox', { name: 'API key or subscription token for Anthropic', exact: true });
  await expect(input).toHaveAttribute('type', 'password');
  await expect(input).toHaveAttribute('placeholder', 'API key or sk-ant-oat… token');
  await input.fill('sk-test-anthropic-1234');
  await input.press('Enter');
  await expect.poll(async () => (await calls(page, 'models.set_key')).map((c) => c.params)).toEqual([{ provider: 'anthropic', key: 'sk-test-anthropic-1234' }]);
  await expect(input).toHaveCount(0);

  // Replace, test, remove (confirmed).
  await row('openai').getByRole('button', { name: 'Replace API key for OpenAI' }).click();
  const openaiInput = row('openai').getByRole('textbox', { name: 'API key for OpenAI', exact: true });
  await expect(openaiInput).toHaveAttribute('placeholder', 'API key');
  await openaiInput.fill('sk-openai-test-9999');
  await row('openai').getByRole('button', { name: 'Save' }).click();
  await expect.poll(async () => (await calls(page, 'models.set_key')).length).toBe(2);
  await row('google').getByRole('button', { name: 'Test Google (Gemini)' }).click();
  await expect.poll(async () => (await calls(page, 'models.test')).map((c) => c.params)).toEqual([{ provider: 'google' }]);
  await row('google').getByRole('button', { name: 'Remove API key for Google (Gemini)' }).click();
  await page.getByRole('alertdialog', { name: /Remove the Google/ }).getByRole('button', { name: 'Remove' }).click();
  await expect.poll(async () => (await calls(page, 'models.remove')).map((c) => c.params)).toEqual([{ provider: 'google' }]);
});

test('models: a pasted subscription token', async ({ page }) => {
  await start(page);
  const settings = await openModels(page);
  const row = settings.locator('[data-provider="anthropic"]');
  const token = 'sk-ant-oat01-test-abcd';
  // The agent daemon's answer: Anthropic has a subscription token.
  const state = structuredClone(fixture.responses['models.list']) as unknown as ModelsState;
  const anthropic = state.providers.find((p) => p.id === 'anthropic')!;
  anthropic.source = 'ui';
  anthropic.key = { kind: 'subscription_token', last4: 'abcd', added_at: 1767592800000, added_by: null };
  await page.evaluate((s) => window.__fixture!.reply('models.set_key', s), state);

  await row.getByRole('button', { name: 'Add API key or subscription token for Anthropic' }).click();
  await row.getByRole('textbox', { name: 'API key or subscription token for Anthropic', exact: true }).fill(token);
  await row.getByRole('button', { name: 'Save' }).click();
  await expect.poll(async () => (await calls(page, 'models.set_key')).map((c) => c.params)).toEqual([{ provider: 'anthropic', key: token }]);
  await expect(row.locator('[data-status]')).toHaveText('token set …abcd');
  await expect(row).toContainText('subscription token …abcd');
  await expect(row.getByRole('button', { name: 'Replace subscription token for Anthropic' })).toHaveText('Replace token');
  await expect(row.getByRole('button', { name: 'Remove subscription token for Anthropic' })).toBeVisible();
  await expect(row.getByRole('button', { name: 'Sign in to Anthropic' })).toBeDisabled();
});

test('models: a custom gateway and the defaults', async ({ page }) => {
  await start(page);
  const settings = await openModels(page);
  await settings.getByRole('button', { name: '+ Gateway' }).click();
  const form = settings.getByRole('group', { name: 'New gateway' });
  await form.getByLabel('Gateway name').fill('team-gw');
  await form.getByLabel('Gateway base URL').fill('https://llm.example.com/v1');
  await form.getByLabel('Gateway models').fill('model-a, model-b');
  await form.getByLabel('Gateway key header').click();
  await page.getByRole('option', { name: 'x-api-key' }).click();
  await expect(form.getByRole('button', { name: 'Save gateway' })).toBeDisabled();
  await form.getByLabel('Gateway API key').fill('gw-secret-5678');
  await form.getByRole('button', { name: 'Save gateway' }).click();
  await expect.poll(async () => (await calls(page, 'models.set_gateway')).map((c) => c.params)).toEqual([
    { provider: 'team-gw', gateway: { base_url: 'https://llm.example.com/v1', models: ['model-a', 'model-b'], auth_header: 'x_api_key' }, key: 'gw-secret-5678' },
  ]);

  // Editing an existing gateway keeps its key unless a new one is typed.
  const gw = settings.locator('[data-provider="gw"]');
  await gw.getByRole('button', { name: 'Edit gateway gw' }).click();
  const edit = settings.getByRole('group', { name: 'Edit gateway gw' });
  await expect(edit.getByLabel('Gateway name')).toBeDisabled();
  await edit.getByLabel('Gateway models').fill('mock-model mock-model-2');
  await edit.getByRole('button', { name: 'Save gateway' }).click();
  await expect.poll(async () => (await calls(page, 'models.set_gateway'))[1]?.params).toMatchObject({ provider: 'gw', key: null, gateway: { models: ['mock-model', 'mock-model-2'] } });

  // Defaults: the model list follows the provider; the server's default is named.
  const defaults = settings.locator('[data-defaults]');
  await expect(defaults.getByLabel('Default provider')).toContainText('Anthropic');
  await defaults.getByLabel('Default model').click();
  await page.getByRole('option', { name: 'Mock Haiku' }).click();
  await defaults.getByLabel('Default thinking level').click();
  await page.getByRole('option', { name: 'low', exact: true }).click();
  await defaults.getByLabel('Default provider').click();
  await expect(page.getByRole('option', { name: 'Server (litellm/mock-large)' })).toBeVisible();
  await page.getByRole('option', { name: 'gw' }).click();
  await expect.poll(async () => (await calls(page, 'models.set_defaults')).map((c) => c.params)).toEqual([
    { defaults: { provider: 'anthropic', model: 'mock-haiku', thinking: 'high' } },
    { defaults: { provider: 'anthropic', model: 'mock-sonnet', thinking: 'low' } },
    { defaults: { provider: 'gw', model: null, thinking: 'high' } },
  ]);
});

test('models: subscription sign-in (method, sign-in page, pasted URL, signed in)', async ({ page }) => {
  await start(page);
  const settings = await openModels(page);
  // A provider with a key here can't sign in (a sign-in would replace it).
  await expect(settings.locator('[data-provider="openai-codex"]').getByRole('button', { name: /^Sign in/ })).toHaveCount(0);
  const row = settings.locator('[data-provider="anthropic"]');
  await row.getByRole('button', { name: 'Sign in to Anthropic' }).click();
  const flow = row.getByRole('group', { name: 'Sign in to Anthropic' });
  await flow.getByRole('button', { name: 'Browser login (default)' }).click();
  await expect.poll(async () => (await calls(page, 'models.login_input')).map((c) => c.params)).toEqual([
    { login_id: '01JAL000000000000000000001', input: 'browser' },
  ]);
  await page.evaluate((s) => window.__fixture!.reply('models.login_wait', s), login(3, { kind: 'open_url', url: 'https://id.example.com/authorize?state=s1', instructions: null }));
  await expect(flow.getByRole('link', { name: 'Open sign-in page ↗' })).toHaveAttribute('href', 'https://id.example.com/authorize?state=s1');
  await flow.getByLabel('Redirect URL or code for Anthropic').fill('http://localhost:53692/callback?code=c1&state=s1');
  await flow.getByRole('button', { name: 'Submit' }).click();
  await expect.poll(async () => (await calls(page, 'models.login_input')).map((c) => (c.params as { input: string }).input)).toEqual([
    'browser',
    'http://localhost:53692/callback?code=c1&state=s1',
  ]);
  const lists = (await calls(page, 'models.list')).length;
  await page.evaluate((s) => window.__fixture!.reply('models.login_wait', s), login(5, { kind: 'done', account: 'alice@example.com' }));
  await expect(page.getByText('Signed in to Anthropic')).toBeVisible();
  await expect.poll(async () => (await calls(page, 'models.list')).length).toBeGreaterThan(lists);
  await expect(flow).toHaveCount(0);

  // A failure shows the provider's message; cancel stops the flow.
  await row.getByRole('button', { name: 'Sign in to Anthropic' }).click();
  await page.evaluate((s) => window.__fixture!.reply('models.login_wait', s), login(3, { kind: 'failed', message: 'invalid_grant' }));
  await expect(flow.getByRole('alert')).toHaveText('Sign-in failed: invalid_grant');
  await flow.getByRole('button', { name: 'Close' }).click();
  await row.getByRole('button', { name: 'Sign in to Anthropic' }).click();
  await flow.getByRole('button', { name: 'Cancel sign-in to Anthropic' }).click();
  await expect.poll(async () => (await calls(page, 'models.login_cancel')).length).toBe(1);
  await expect(flow).toHaveCount(0);

  // Signed in elsewhere: sign out after confirming.
  await settings.locator('[data-provider="openai-codex"]').getByRole('button', { name: 'Sign out of ChatGPT (Codex)' }).click();
  await page.getByRole('alertdialog', { name: /Sign out of ChatGPT/ }).getByRole('button', { name: 'Sign out' }).click();
  await expect.poll(async () => (await calls(page, 'models.sign_out')).map((c) => c.params)).toEqual([{ provider: 'openai-codex' }]);
});

test('models: phone width', async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 }, hasTouch: true });
  await start(page);
  const settings = await openModels(page, true);
  const row = settings.locator('[data-provider="openai"]');
  await expect(row.getByRole('button', { name: 'Replace API key for OpenAI' })).toBeVisible();
  await expect(settings.locator('[data-defaults]').getByLabel('Default thinking level')).toBeVisible();
  // Nothing wider than the screen.
  const overflow = await settings.evaluate((el) =>
    [...el.querySelectorAll('[data-provider], [data-defaults], [data-provider] button')].some((e) => e.getBoundingClientRect().right > window.innerWidth + 1),
  );
  expect(overflow).toBe(false);
  await row.getByRole('button', { name: 'Replace API key for OpenAI' }).click();
  await expect(row.getByRole('textbox', { name: 'API key for OpenAI', exact: true })).toBeVisible();
  await page.close();
});
