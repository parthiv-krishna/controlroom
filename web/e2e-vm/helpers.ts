import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { expect, type Browser, type BrowserContext, type Locator, type Page } from '@playwright/test';

/** A required E2E_* setting. */
export function env(name: string): string {
  const value = process.env[name];
  if (value === undefined) throw new Error(`${name} is not set (the VM test sets it)`);
  return value;
}

/**
 * A step done outside the browser by the VM test script (nix/tests/e2e.py): writes
 * `<E2E_GATES>/<name>.reached` with `payload`, then waits until the script releases it.
 */
export async function step(name: string, payload: Record<string, unknown> = {}): Promise<void> {
  const gates = env('E2E_GATES');
  writeFileSync(`${gates}/${name}.tmp`, JSON.stringify(payload));
  renameSync(`${gates}/${name}.tmp`, `${gates}/${name}.reached`);
  const released = `${gates}/${name}.release`;
  await expect.poll(() => existsSync(released), { message: `step ${name} released`, timeout: 10 * 60_000, intervals: [250] }).toBe(true);
}

/** A new browser (its own device) signed in with the dev token, named `device`. */
export async function signIn(browser: Browser, device: string): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await login(page, device);
  return { context, page };
}

/** Signs `page` in with the server's dev token and names its device on first load. */
export async function login(page: Page, device: string): Promise<void> {
  const token = readFileSync(env('E2E_TOKEN_FILE'), 'utf8').trim();
  await page.goto(`/auth/dev-token?token=${encodeURIComponent(token)}`);
  await expect(page.getByRole('listbox', { name: 'Session list' })).toBeVisible();
  const naming = page.getByRole('region', { name: 'Name this device' });
  await naming.getByLabel('Device name').fill(device);
  await naming.getByRole('button', { name: 'Save' }).click();
  await expect(naming).toHaveCount(0);
}

/** Opens Settings › Machines. */
export async function openMachines(page: Page): Promise<Locator> {
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Machines' }).click();
  return settings;
}

/** A machine's card in Settings › Machines. */
export function machineCard(settings: Locator, name: string): Locator {
  return settings.locator('.card[data-machine]').filter({ has: settings.page().locator('b').getByText(name, { exact: true }) });
}

/** Closes Settings. */
export async function closeSettings(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Close settings' }).click();
}

/** A session's row in the session list. */
export function sessionRow(page: Page, name: string): Locator {
  return page.getByRole('listbox', { name: 'Session list' }).getByRole('option').filter({ has: page.locator('.nm').getByText(name, { exact: true }) });
}

/** Creates a session from the new-session dialog and waits until it is open. */
export async function newSession(
  page: Page,
  opts: { machine?: string; cwd: string; name: string; level: 'Strict' | 'Relaxed' | 'Off'; prompt: string },
): Promise<void> {
  await page.getByRole('button', { name: '+ New session' }).click();
  const dialog = page.getByRole('dialog', { name: 'New session' });
  if (opts.machine) {
    await dialog.getByRole('button', { name: 'Machine' }).click();
    await dialog
      .getByRole('listbox', { name: 'Machine' })
      .getByRole('option')
      .filter({ has: page.locator('.dd-l').getByText(opts.machine, { exact: true }) })
      .click();
  }
  await expect(dialog.getByRole('radio', { name: 'pi' })).toBeChecked();
  await dialog.getByLabel('Working directory').fill(opts.cwd);
  await dialog.getByLabel('Name').fill(opts.name);
  await dialog.getByRole('button', { name: 'Sandbox level' }).click();
  await dialog
    .getByRole('listbox', { name: 'Sandbox level' })
    .getByRole('option')
    .filter({ has: page.locator('.dd-l').getByText(opts.level, { exact: true }) })
    .click();
  await dialog.getByLabel('First message').fill(opts.prompt);
  await dialog.getByRole('button', { name: 'Create session' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(header(page).locator('.et.heading')).toHaveText(opts.name);
}

/** The open session's header (title, status, badges, actions). */
export function header(page: Page): Locator {
  return page.locator('[data-pane="header"]');
}

/** The open session's timeline. */
export function timeline(page: Page): Locator {
  return page.locator('[data-pane="session"][aria-label="Timeline"]');
}

/** Sends a message from the open session's composer. */
export async function send(page: Page, text: string): Promise<void> {
  await page.getByLabel('Message').fill(text);
  await page.getByLabel('Message').press('Enter');
  await expect(page.getByLabel('Message')).toHaveValue('');
}
