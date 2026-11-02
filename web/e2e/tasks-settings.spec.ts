import { expect, test } from '@playwright/test';
import { M, calls, start } from './helpers';

test('tasks board: drag a card to another column', async ({ page }) => {
  await start(page);
  await page.locator('body').press('t');
  const board = page.getByRole('region', { name: 'Tasks' });
  const card = board.locator('[data-task="01JAT000000000000000000006"]');
  await card.dragTo(board.getByRole('region', { name: 'Review' }));
  await expect.poll(async () => (await calls(page, 'tasks.move')).map((c) => c.params)).toContainEqual(
    expect.objectContaining({ task_id: '01JAT000000000000000000006', status: 'review' }),
  );
});

test('tasks: the detail pane and the new-task dialog (everything in one go)', async ({ page }) => {
  await start(page);
  await page.locator('body').press('t');
  const board = page.getByRole('region', { name: 'Tasks' });
  await board.locator('[data-task="01JAT000000000000000000001"]').getByText('Fix random logouts').click();
  const detail = page.getByLabel('Task details');
  await expect(detail.getByText('Agent sessions')).toBeVisible();
  await detail.getByLabel('Add checklist item').fill('Ship it');
  await detail.getByLabel('Add checklist item').press('Enter');
  await board.getByRole('button', { name: '+ Task' }).click();
  const dialog = page.getByRole('dialog', { name: 'New task' });
  await dialog.getByPlaceholder('What needs doing?').fill('Write the changelog');
  await dialog.getByLabel('Add checklist item').fill('Collect PRs');
  await dialog.getByLabel('Add checklist item').press('Enter');
  await dialog.getByRole('button', { name: '↵ Create task' }).click();
  const [create] = await calls(page, 'tasks.create');
  expect(create.params).toMatchObject({ title: 'Write the changelog', status: 'todo', priority: 'p2', checklist: ['Collect PRs'] });
  const [add] = await calls(page, 'tasks.checklist_add');
  expect(add.params).toMatchObject({ task_id: '01JAT000000000000000000001', text: 'Ship it' });
});

test('new session dialog: machine, agent, directory and sandbox', async ({ page }) => {
  await start(page);
  await page.getByRole('button', { name: '+ New session' }).click();
  const dialog = page.getByRole('dialog', { name: 'New session' });
  await expect(dialog.getByLabel('Machine')).toContainText('buildbox');
  await expect(dialog.getByRole('radio', { name: 'pi' })).toBeChecked();
  await dialog.getByLabel('Working directory').fill('~/src/acme-api');
  await dialog.getByLabel('Sandbox level').click();
  await dialog.getByRole('option', { name: /^Off/ }).click();
  await expect(dialog.getByText('no sandbox')).toBeVisible();
  await dialog.getByLabel('Name').fill('try the fix');
  await dialog.getByLabel('First message').fill('reproduce the bug');
  await dialog.getByRole('button', { name: '↵ Create session' }).click();
  const [create] = await calls(page, 'session.create');
  expect(create.params).toMatchObject({ machine_id: M.buildbox, agent: 'pi', cwd: '~/src/acme-api', name: 'try the fix', sandbox_level: 'off', task_ids: [] });
  // The first message travels in the create call (the server links tasks and sends it once).
  expect((create.params as { prompt: { content: unknown } }).prompt.content).toEqual([{ kind: 'text', text: 'reproduce the bug' }]);
  expect(await calls(page, 'session.prompt')).toEqual([]);
});

test('settings: tabs, the add-machine flow, devices and sandbox defaults', async ({ page }) => {
  await start(page);
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByText('host key changed', { exact: true })).toBeVisible();

  await settings.getByRole('button', { name: '+ Add machine' }).click();
  await settings.getByPlaceholder('devbox.example.com').fill('newbox.example.com');
  await settings.getByPlaceholder('alice').fill('alice');
  await settings.getByRole('button', { name: /Connect/ }).click();
  await expect(settings.getByText('Confirm host key')).toBeVisible();
  await settings.getByRole('button', { name: /Trust and continue/ }).click();
  await expect(settings.getByText('Permission denied')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(settings.getByRole('heading', { name: 'Add machine' })).toBeVisible();

  for (const tab of ['SSH keys', 'Notifications', 'Devices', 'Sandbox', 'Contacts', 'Schedules', 'Keyboard', 'Appearance']) {
    await settings.getByRole('tab', { name: tab }).click();
    await expect(settings.getByRole('tab', { name: tab })).toHaveAttribute('aria-selected', 'true');
  }
  await settings.getByRole('tab', { name: 'Devices' }).click();
  await expect(settings.locator('[data-device="device-phone"]')).toContainText('push on');
  await settings.getByRole('tab', { name: 'Sandbox' }).click();
  await settings.getByLabel('Add allowed domain for devbox').fill('example.com');
  await settings.getByLabel('Add allowed domain for devbox').press('Enter');
  const [change] = await calls(page, 'sandbox.set_profile');
  expect(change.params).toMatchObject({ scope: { kind: 'machine', machine_id: M.devbox }, change: { kind: 'add_domain', domain: 'example.com' } });

  await settings.getByRole('tab', { name: 'Appearance' }).click();
  await settings.getByRole('radio', { name: 'Dark' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await settings.getByRole('button', { name: 'Expand to full page' }).click();
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Projects' })).toBeVisible();
});

test('project view: Save as view from a location filter', async ({ page }) => {
  await start(page);
  await page.getByRole('button', { name: 'Filter' }).click();
  const menu = page.getByRole('dialog', { name: 'Filter' });
  await menu.getByRole('button', { name: 'Location' }).click();
  await menu.getByText('acme-api', { exact: true }).click();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Save as view' }).click();
  const dialog = page.getByRole('dialog', { name: 'Project view' });
  await dialog.getByLabel('Name').fill('API');
  await dialog.getByRole('button', { name: '↵ Save' }).click();
  const [create] = await calls(page, 'projects.create');
  expect(create.params).toMatchObject({ name: 'API', location_prefixes: [{ machine_id: M.buildbox, path_prefix: '~/src/acme-api' }] });
});
