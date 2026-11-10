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

/** Picks `value` in an app dropdown labelled `label` (inside `scope`). */
export async function choose(scope: Locator, label: string, value: string | RegExp): Promise<void> {
  await scope.getByRole('button', { name: label, exact: true }).click();
  const option = scope.getByRole('listbox', { name: label, exact: true }).getByRole('option').filter({ has: scope.page().locator('.dd-l').getByText(value, { exact: true }) });
  await option.click();
}

/** A session's row in the session list. */
export function sessionRow(page: Page, name: string): Locator {
  return page.getByRole('listbox', { name: 'Session list' }).getByRole('option').filter({ has: page.locator('.nm').getByText(name, { exact: true }) });
}

/** Opens a session from the list (in "All", so the project scope doesn't hide it). */
export async function openSession(page: Page, name: string): Promise<void> {
  await page.getByRole('navigation', { name: 'Projects' }).getByRole('button', { name: 'All projects' }).click();
  await sessionRow(page, name).click();
  await expect(header(page).locator('.et.heading')).toHaveText(name);
}

/** A workspace of a new session: a machine and a directory on it. */
export interface Workspace {
  machine: string;
  path: string;
}

/**
 * Creates a session from the new-session dialog (its workspaces in order, the first the primary)
 * and waits until it is open. Without a prompt nothing runs yet.
 */
export async function newSession(
  page: Page,
  opts: { workspaces: Workspace[]; name: string; level: 'Strict' | 'Relaxed' | 'Off'; prompt?: string; overlap?: string },
): Promise<void> {
  await page.getByRole('button', { name: 'New session', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'New session' });
  for (const [i, ws] of opts.workspaces.entries()) {
    if (i > 0) await dialog.getByRole('button', { name: '+ Workspace' }).click();
    await choose(dialog, `Machine ${i + 1}`, ws.machine);
    await dialog.getByLabel(`Directory ${i + 1}`).fill(ws.path);
  }
  // A directory another open session uses is allowed, with a warning.
  if (opts.overlap) await expect(dialog.locator('[data-overlap]')).toContainText(`Also used by ${opts.overlap}`);
  else await expect(dialog.locator('[data-overlap]')).toHaveCount(0);
  await dialog.getByLabel('Name').fill(opts.name);
  await choose(dialog, 'Sandbox level', opts.level);
  if (opts.prompt) await dialog.getByLabel('Prompt').fill(opts.prompt);
  await dialog.getByRole('button', { name: /Create$/ }).click();
  await expect(dialog).toHaveCount(0);
  await expect(header(page).locator('.et.heading')).toHaveText(opts.name);
}

/** The open session's header (title, status, workspaces, badges, actions). */
export function header(page: Page): Locator {
  return page.locator('[data-pane="header"]');
}

/** The open session's timeline. */
export function timeline(page: Page): Locator {
  return page.locator('[data-pane="session"][aria-label="Timeline"]');
}

/** The open session's request cards of a kind (a CSS suffix): pinned above the composer while open, then in the timeline. */
export function requestCards(page: Page, kind: string): Locator {
  return page.getByRole('region', { name: 'Requests', exact: true }).or(timeline(page)).locator(`.rq-${kind}`);
}

/** The open session's last agent message. */
export function lastReply(page: Page): Locator {
  return timeline(page).locator('.msg.r-agent').last();
}

/** The open session's tool calls whose title contains `title`. */
export function toolCalls(page: Page, title: string | RegExp): Locator {
  return timeline(page).locator('.tool').filter({ has: page.locator('.head .title').getByText(title) });
}

/** The banner of an unreachable machine in the open session. */
export function machineBanner(page: Page, machine: string): Locator {
  return page.locator(`[data-machine-banner="${machine}"]`);
}

/**
 * Waits until no machine banner is left in the open session, pressing "Retry" while the
 * server waits out its reconnect backoff (it may also reconnect on its own first).
 */
export async function waitReconnected(page: Page, machine: string): Promise<void> {
  const banner = machineBanner(page, machine);
  const retry = banner.getByRole('button', { name: /^Retry connecting/ });
  await expect(async () => {
    if (await retry.isVisible()) await retry.click({ timeout: 1_000 });
    await expect(page.locator('[data-machine-banner]')).toHaveCount(0, { timeout: 5_000 });
  }).toPass({ timeout: 300_000 });
}

/** Sends a message from the open session's composer. */
export async function send(page: Page, text: string): Promise<void> {
  await page.getByLabel('Message').fill(text);
  await page.getByLabel('Message').press('Enter');
  await expect(page.getByLabel('Message')).toHaveValue('');
}

/** Waits until the open session's run has ended. */
export async function settled(page: Page): Promise<void> {
  await expect(header(page).locator('.status')).toHaveText('Complete', { timeout: 120_000 });
}
