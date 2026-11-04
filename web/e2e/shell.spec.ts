import { expect, test } from '@playwright/test';
import { S, calls, start } from './helpers';

test('rail: projects filter the list; panes toggle and their pins persist', async ({ page }) => {
  await start(page);
  const list = page.getByRole('listbox', { name: 'Session list' });
  await expect(list.getByText('inbox triage')).toBeVisible();
  await page.getByRole('button', { name: 'Docs', exact: true }).click();
  await expect(list.getByText('docs site refresh')).toBeVisible();
  await expect(list.getByText('inbox triage')).toHaveCount(0);
  await page.getByRole('button', { name: 'All projects' }).click();

  // notifications and tasks: pin with ] and t, unpin the list with [
  await page.locator('body').press(']');
  await expect(page.getByRole('complementary', { name: 'Notifications' })).toBeVisible();
  await page.locator('body').press('t');
  await expect(page.getByRole('region', { name: 'Tasks' })).toBeVisible();
  await page.locator('body').press('[');
  await expect(page.getByRole('button', { name: 'Pin sessions' })).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole('complementary', { name: 'Notifications' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Tasks' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Unpin notifications' })).toBeVisible();
});

test('session list: search, filter by state and group by machine (a session under every machine it touches)', async ({ page }) => {
  await start(page);
  const list = page.getByRole('listbox', { name: 'Session list' });
  await page.getByLabel('Search sessions').fill('logouts');
  await expect(list.getByRole('option')).toHaveCount(2);
  await page.getByLabel('Search sessions').fill('');

  await page.getByRole('button', { name: 'Filter' }).click();
  await page.getByRole('dialog', { name: 'Filter' }).getByText('Archived').click();
  await expect(list.getByText('rotate secrets')).toBeVisible();
  await expect(list.getByText('inbox triage')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Remove filter' }).click();
  await expect(list.getByText('inbox triage')).toBeVisible();

  await page.getByRole('button', { name: /Group/ }).click();
  await page.getByRole('menuitemradio', { name: 'Machine' }).click();
  await expect(list.locator('[data-group^="m:"]')).toHaveText([/buildbox/, /devbox/, /gpubox/]);
  // Two workspaces: listed under both machines.
  await expect(list.getByRole('option', { name: /auth token refresh race/ })).toHaveCount(2);
  // gpubox is unreachable: its group offers no new session.
  await expect(list.locator('[data-group^="m:"]').filter({ hasText: 'gpubox' }).getByRole('button', { name: /New session/ })).toHaveCount(0);

  // Filter by machine: the sessions with a workspace there.
  await page.getByRole('button', { name: 'Filter' }).click();
  const filter = page.getByRole('dialog', { name: 'Filter' });
  await filter.getByRole('button', { name: 'Machine' }).click();
  await filter.getByText('gpubox').click();
  await page.keyboard.press('Escape');
  // Grouped by machine, it shows under both of its machines.
  await expect(list.getByRole('option')).toHaveText([/dataset dedupe/, /dataset dedupe/]);
});

test('rail: Unassigned holds the sessions that no project claims', async ({ page }) => {
  await start(page);
  const list = page.getByRole('listbox', { name: 'Session list' });
  await page.getByRole('navigation', { name: 'Projects' }).getByRole('button', { name: 'Unassigned' }).click();
  await expect(page.getByRole('region', { name: 'Sessions' }).getByText('Unassigned', { exact: true })).toBeVisible();
  await expect(list.getByText('inbox triage')).toBeVisible();
  await expect(list.getByText('auth token refresh race')).toHaveCount(0);
  // Linking a session to a task is what moves it into the task's projects.
  await page.getByRole('navigation', { name: 'Projects' }).getByRole('button', { name: 'Acme API' }).click();
  await expect(list.getByText('auth token refresh race')).toBeVisible();
  await expect(list.getByText('inbox triage')).toHaveCount(0);
});

test('subagents: a filter matching only a child keeps the parent as a dimmed ancestor', async ({ page }) => {
  await start(page);
  await page.getByLabel('Search sessions').fill('64k');
  const list = page.getByRole('listbox', { name: 'Session list' });
  await expect(list.getByRole('option', { name: /benchmark coordinator/ })).toHaveClass(/ancestor/);
  await expect(list.getByRole('option', { name: /sweep: 64k/ })).not.toHaveClass(/ancestor/);
  await expect(list.getByText('sweep: 8k–32k')).toHaveCount(0);
});

test('drag a session row onto a task group links it', async ({ page }) => {
  await start(page);
  const row = page.getByRole('option', { name: /inbox triage/ });
  await row.dragTo(page.locator('[data-group^="t:"]').filter({ hasText: 'Release v0.2' }));
  await expect.poll(async () => (await calls(page, 'tasks.link')).map((c) => c.params)).toContainEqual({
    task_id: '01JAT000000000000000000003',
    session_id: '01JAS000000000000000000018',
  });
});

test('keyboard: j/k move through sessions, ? shows help, ⌘K opens the palette', async ({ page }) => {
  await start(page, S.auth);
  await page.locator('body').press('j');
  await expect(page.locator('h2, .head').getByText('upgrade tokio to 1.47').first()).toBeVisible();
  await page.locator('body').press('k');
  await expect(page.getByRole('option', { name: /auth token refresh race/ })).toHaveAttribute('aria-selected', 'true');
  await page.locator('body').press('?');
  await expect(page.getByRole('dialog', { name: 'Keyboard' })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.keyboard.press('ControlOrMeta+Shift+K');
  await expect(page.getByRole('dialog', { name: 'Command palette' })).toBeVisible();
  await page.keyboard.type('release');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('option', { name: /release 0.2 checklist/ })).toHaveAttribute('aria-selected', 'true');
});

test('phone: list, full-screen conversation with its panels, back', async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 }, hasTouch: true });
  await start(page);
  await expect(page.getByRole('navigation', { name: 'Projects' })).toBeHidden();
  await page.getByRole('button', { name: 'Projects' }).click();
  await expect(page.getByRole('dialog', { name: 'Projects' })).toBeVisible();
  await page.getByRole('dialog', { name: 'Projects' }).getByRole('button', { name: /Controlroom/ }).click();
  await page.getByRole('option', { name: /release 0.2 checklist/ }).click();
  await expect(page.getByLabel('Todo', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '‹ Sessions' })).toBeVisible();
  await page.getByRole('button', { name: '‹ Sessions' }).click();
  await page.getByRole('button', { name: 'Tasks' }).click();
  await expect(page.getByRole('region', { name: 'In progress' })).toBeVisible();
  await page.getByRole('button', { name: 'Notifications' }).click();
  await expect(page.getByRole('complementary', { name: 'Notifications' })).toBeVisible();
  await page.close();
});

test('offline: a push deep link pre-renders, then "can\'t reach server"', async ({ page }) => {
  await page.goto(`/?offline#/s/${S.flaky}?n=flaky%20e2e%20tests&m=devbox&st=Working`);
  await expect(page.getByRole('heading', { name: 'flaky e2e tests' })).toBeVisible();
  await expect(page.getByText('devbox · Working')).toBeVisible();
  await expect(page.getByText("Can't reach server")).toBeVisible();
});

test('device naming: first load asks, Save renames this device', async ({ page }) => {
  await start(page);
  const prompt = page.getByRole('region', { name: 'Name this device' });
  await prompt.getByLabel('Device name').fill('Laptop');
  await prompt.getByRole('button', { name: 'Save' }).click();
  await expect(prompt).toHaveCount(0);
  const [rename] = await calls(page, 'devices.rename');
  expect(rename.params).toMatchObject({ label: 'Laptop' });
  await page.reload();
  await expect(page.getByRole('region', { name: 'Name this device' })).toHaveCount(0);
});
