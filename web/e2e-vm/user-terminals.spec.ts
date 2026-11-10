import { expect, test } from '@playwright/test';
import { lastReply, login, openSession, send, settled } from './helpers';

// The user's own terminals on devbox (over the controller's terminal streams): an unsandboxed one
// writes outside the workspace, a sandboxed one can't. Then the agent lists its shells (not ours)
// and tries a command in ours (refused); the test script checks the files, what the model saw
// and the logs. Closing our idle terminal ends it.
test('your terminals: unsandboxed runs outside nono, sandboxed is confined, the agent sees neither', async ({ page }) => {
  await login(page, 'Laptop');
  await openSession(page, 'two');
  const dock = page.getByRole('region', { name: 'Terminals' });
  const tabs = dock.getByRole('tablist', { name: 'Shells' });

  // Unsandboxed, after its confirm.
  await dock.getByRole('button', { name: 'New terminal' }).click();
  await page.getByRole('menu', { name: 'New terminal' }).getByRole('menuitem', { name: 'Unsandboxed… devbox:~/work/api' }).click();
  const confirm = page.getByRole('alertdialog', { name: 'Open an unsandboxed terminal?' });
  await expect(confirm).toContainText('Runs as alice on devbox without the sandbox.');
  await confirm.getByRole('button', { name: 'Open unsandboxed' }).click();
  await expect(tabs.getByRole('tab', { name: 'unsandboxed devbox' })).toHaveAttribute('aria-selected', 'true');
  await expect(dock.getByRole('status').filter({ hasText: 'UNSANDBOXED' })).toContainText('as alice on devbox');
  const rows = dock.locator('.xterm-rows');
  await expect(rows).toContainText('$');
  await dock.locator('.xterm').click();
  await page.keyboard.type('echo outside > ~/outside-unsandboxed.txt && echo wrote-$((40+2))');
  await page.keyboard.press('Enter');
  await expect(rows).toContainText('wrote-42');

  // Sandboxed, like the agent's shells.
  await dock.getByRole('button', { name: 'New terminal' }).click();
  await page.getByRole('menu', { name: 'New terminal' }).getByRole('menuitem', { name: 'devbox:~/work/api', exact: true }).click();
  await expect(tabs.getByRole('tab', { name: 'you devbox-2' })).toHaveAttribute('aria-selected', 'true');
  await expect(rows).toContainText('$');
  await dock.locator('.xterm').click();
  await page.keyboard.type('(echo x > ~/planted-by-you.txt) 2>/dev/null || echo con""fined');
  await page.keyboard.press('Enter');
  await expect(rows).toContainText('confined');

  // The agent lists only its shells, and can't run in ours.
  await send(page, 'Which shells do you have?');
  await expect(lastReply(page)).toHaveText('Only my own shells.');
  await settled(page);

  // Closing our idle terminal ends it; the other one stays.
  await tabs.getByRole('button', { name: 'End terminal devbox-2' }).click();
  await expect(tabs.getByRole('tab', { name: 'you devbox-2' })).toHaveCount(0);
  await expect(tabs.getByRole('tab', { name: 'unsandboxed devbox' })).toHaveCount(1);
});
