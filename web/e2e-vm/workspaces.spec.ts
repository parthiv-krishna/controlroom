import { expect, test } from '@playwright/test';
import { header, lastReply, login, newSession, sessionRow, settled, toolCalls } from './helpers';

// One session with workspaces on both machines: pi (on the server, under nono) reads a file on
// devbox, edits one on buildbox, and runs commands in each machine's named shell, whose state
// (cwd, variables) persists between calls; a write outside devbox's workspace is confined by
// the host's sandbox (the test script checks the files and what the model saw). The session
// starts Unassigned; linking it to a new task of a new project moves it there.
test('a session with workspaces on two machines, then from Unassigned to a task', async ({ page }) => {
  await login(page, 'Laptop');
  await newSession(page, {
    workspaces: [
      { machine: 'devbox', path: '~/work/api' },
      { machine: 'buildbox', path: '~/work/web' },
    ],
    name: 'two',
    level: 'Strict',
    prompt: 'Look around both machines',
  });

  // Both workspaces, the primary first.
  const pills = header(page).getByLabel('Workspaces').locator('[data-workspace]');
  await expect(pills).toHaveText([/^devbox:~\/work\/api/, /^buildbox:~\/work\/web/]);
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
  await header(page).getByRole('button', { name: '◆ link to task' }).click();
  await page.getByRole('dialog', { name: 'Link to tasks' }).getByRole('button', { name: '+ New task from this session' }).click();
  const task = page.getByRole('dialog', { name: 'New task' });
  await task.getByPlaceholder('What needs doing?').fill('Ship the web app');
  await task.getByRole('combobox', { name: 'Projects' }).fill('Web');
  await task.getByRole('combobox', { name: 'Projects' }).press('Enter');
  await task.getByRole('button', { name: 'Create task' }).click();
  await expect(task).toHaveCount(0);
  await expect(header(page).getByRole('button', { name: '◆ Ship the web app' })).toBeVisible();

  // Now it is in the project, and no longer Unassigned.
  await rail.getByRole('button', { name: 'Web' }).click();
  await expect(sessionRow(page, 'two')).toBeVisible();
  await expect(sessionRow(page, 'build')).toHaveCount(0);
  await rail.getByRole('button', { name: 'Unassigned' }).click();
  await expect(sessionRow(page, 'build')).toBeVisible();
  await expect(sessionRow(page, 'two')).toHaveCount(0);
});
