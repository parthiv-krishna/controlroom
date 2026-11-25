import { expect, test } from '@playwright/test';
import { choose, header, sessionAction, lastReply, login, newSession, send, sessionRow, settled, toolCalls } from './helpers';

// A session on devbox gets a second workspace on buildbox through its ⋯ menu; then pi (on the
// server, under nono) reads a file on devbox, edits one on buildbox, and runs commands in each
// machine's named shell, whose state (cwd, variables) persists between calls; a write outside
// devbox's workspace is confined by the host's sandbox (the test script checks the files and what
// the model saw). The session starts Unassigned; linking it to a new task of a new project moves
// it there.
test('a session on devbox adds a workspace on buildbox, then from Unassigned to a task', async ({ page }) => {
  await login(page, 'Laptop');
  await newSession(page, { workspaces: [{ machine: 'devbox', path: '~/work/api' }], name: 'two', level: 'Strict' });
  await expect(header(page).getByLabel('Workspaces', { exact: true }).locator('[data-workspace]')).toHaveText([/^devbox:~\/work\/api/]);

  // ⋯ › + Workspace: buildbox:~/work/web.
  await sessionAction(page, 'Add workspace');
  const add = page.getByRole('dialog', { name: 'Add workspace' });
  await choose(add, 'Machine', 'buildbox');
  await add.getByLabel('Directory').fill('~/work/web');
  await add.getByRole('button', { name: /Add$/ }).click();
  await expect(add).toHaveCount(0);

  // One chip for both, the primary first in its list.
  const chip = header(page).getByRole('button', { name: '2 workspaces', exact: true });
  await chip.click();
  const rows = page.getByRole('dialog', { name: 'Session workspaces' }).locator('[data-workspace]');
  await expect(rows).toHaveText([/devbox:~\/work\/api\s*primary/, /buildbox:~\/work\/web/]);
  await expect(rows.nth(1)).toContainText(/\bconnected\s*$/);
  await chip.click();
  await expect(rows).toHaveCount(0);

  await send(page, 'Look around both machines');
  await expect(lastReply(page)).toHaveText('Both machines work.');
  for (const title of ['Read devbox:notes.txt', 'Edit buildbox:app.js', '# cd src', '# greet', '# where']) {
    await expect(toolCalls(page, title)).toHaveClass(/s-completed/);
  }
  await settled(page);
  // Each workspace has its shell in the terminal dock.
  const tabs = page.getByRole('region', { name: 'Terminals' }).getByRole('tablist', { name: 'Shells' });
  await expect(tabs.getByRole('tab')).toHaveText([/^devbox/, /^buildbox/]);

  // No task links the session: it is Unassigned.
  const rail = page.getByRole('navigation', { name: 'Projects' });
  await rail.getByRole('button', { name: 'Unassigned' }).click();
  await expect(sessionRow(page, 'two')).toBeVisible();

  // A new project, and a task in it made from the session.
  await rail.getByRole('button', { name: 'New project' }).click();
  const project = page.getByRole('dialog', { name: 'Project' });
  await project.getByLabel('Name').fill('Web');
  await project.getByRole('button', { name: 'Save' }).click();
  await expect(project).toHaveCount(0);
  await rail.getByRole('button', { name: 'Unassigned' }).click();
  await sessionRow(page, 'two').click();
  await sessionAction(page, 'Link to task…');
  await page.getByRole('dialog', { name: 'Link to tasks' }).getByRole('button', { name: '+ New task' }).click();
  const task = page.getByRole('dialog', { name: 'New task' });
  await task.getByPlaceholder('What needs doing?').fill('Ship the web app');
  await task.getByRole('combobox', { name: 'Projects' }).fill('Web');
  await task.getByRole('combobox', { name: 'Projects' }).press('Enter');
  await task.getByRole('button', { name: /Create$/ }).click();
  await expect(task).toHaveCount(0);
  await expect(header(page).getByRole('button', { name: 'Task Ship the web app' })).toBeVisible();

  // Now it is in the project, and no longer Unassigned.
  await rail.getByRole('button', { name: 'Web' }).click();
  await expect(sessionRow(page, 'two')).toBeVisible();
  await expect(sessionRow(page, 'build')).toHaveCount(0);
  await rail.getByRole('button', { name: 'Unassigned' }).click();
  await expect(sessionRow(page, 'build')).toBeVisible();
  await expect(sessionRow(page, 'two')).toHaveCount(0);
});
