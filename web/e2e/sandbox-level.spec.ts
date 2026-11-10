import { expect, test, type Page } from '@playwright/test';
import { S, calls, start } from './helpers';

// A session's sandbox level, changed from the header's sandbox popover (the fixture's state doesn't
// change; the calls are checked).

/** Opens the header's sandbox popover. */
async function openSandbox(page: Page, badge: RegExp) {
  await page.locator('[data-pane="header"]').getByRole('button', { name: badge }).click();
  const pop = page.getByRole('dialog', { name: 'Sandbox' });
  await expect(pop).toBeVisible();
  return pop;
}

const levels = async (page: Page) => (await calls(page, 'session.set_sandbox_level')).map((c) => c.params as { id: string; level: string | null });

test('sandbox level: the picker, terse confirms, off in red', async ({ page }) => {
  await start(page, S.auth);
  const pop = await openSandbox(page, /^strict/);
  const picker = pop.getByRole('radiogroup', { name: 'Sandbox level' });
  await expect(picker.getByRole('radio', { name: 'Defaults (strict)' })).toBeChecked();
  await expect(picker.getByRole('radio')).toHaveText(['Defaults (strict)', 'Strict', 'Relaxed', 'Off']);

  // The level it already runs at only becomes explicit: nothing restarts, so nothing to confirm.
  await picker.getByRole('radio', { name: 'Strict', exact: true }).click();
  await expect.poll(() => levels(page)).toEqual([expect.objectContaining({ id: S.auth, level: 'strict' })]);
  await expect(page.getByRole('alertdialog')).toHaveCount(0);

  // Another level restarts the agent: one terse confirm (cancel sends nothing).
  await picker.getByRole('radio', { name: 'Relaxed', exact: true }).click();
  let confirm = page.getByRole('alertdialog', { name: 'Restart the agent with relaxed?' });
  await expect(confirm).toContainText('A running turn stops; the agent’s shells restart.');
  await expect(confirm).toContainText('Running jobs and your terminals keep the old sandbox until they end.');
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  expect(await levels(page)).toHaveLength(1);
  await (await openSandbox(page, /^strict/)).getByRole('radio', { name: 'Relaxed', exact: true }).click();
  await page.getByRole('alertdialog', { name: 'Restart the agent with relaxed?' }).getByRole('button', { name: 'Restart' }).click();
  await expect.poll(async () => (await levels(page)).at(-1)).toMatchObject({ level: 'relaxed' });

  // Off: the red warning.
  const off = (await openSandbox(page, /^strict/)).getByRole('radio', { name: 'Off', exact: true });
  await expect(off).toHaveClass(/danger/);
  await off.click();
  confirm = page.getByRole('alertdialog', { name: 'Restart the agent without a sandbox?' });
  await expect(confirm).toContainText('Not sandboxed: the agent can read and write anything your user can.');
  await confirm.getByRole('button', { name: 'Turn off and restart' }).click();
  await expect.poll(async () => (await levels(page)).at(-1)).toMatchObject({ level: 'off' });
});

test('sandbox level: back to the defaults, and the timeline divider', async ({ page }) => {
  await start(page, S.deploy);
  const pop = await openSandbox(page, /no sandbox/);
  await expect(pop).toContainText('Chosen for this session');
  await expect(pop).toContainText('Not sandboxed: the agent can read and write anything your user can.');
  const picker = pop.getByRole('radiogroup', { name: 'Sandbox level' });
  await expect(picker.getByRole('radio', { name: 'Off', exact: true })).toBeChecked();
  await picker.getByRole('radio', { name: 'Defaults (strict)' }).click();
  await page.getByRole('alertdialog', { name: 'Restart the agent with the defaults (strict)?' }).getByRole('button', { name: 'Restart' }).click();
  await expect.poll(async () => (await levels(page)).at(-1)).toMatchObject({ id: S.deploy, level: null });

  // The level change is a thin divider in the timeline, off in red.
  const divider = page.getByRole('main').getByRole('separator').filter({ has: page.locator('[data-sandbox="off"]') });
  await expect(divider).toHaveText('sandbox → off');
});

test('sandbox level: phone width', async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  await start(page);
  await page.goto(`/?paused#/s/${S.deploy}`);
  const pop = await openSandbox(page, /no sandbox/);
  const radios = pop.getByRole('radio');
  await expect(radios).toHaveCount(4);
  for (const radio of await radios.all()) {
    const box = await radio.boundingBox();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(390);
  }
  await pop.getByRole('radio', { name: 'Relaxed', exact: true }).click();
  const confirm = page.getByRole('alertdialog', { name: 'Restart the agent with relaxed?' });
  await expect(confirm.getByRole('button', { name: 'Restart' })).toBeInViewport();
  await confirm.getByRole('button', { name: 'Restart' }).click();
  await expect.poll(async () => (await levels(page)).at(-1)).toMatchObject({ level: 'relaxed' });
  await page.close();
});

