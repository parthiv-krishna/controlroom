import { expect, test } from '@playwright/test';
import { header, lastReply, login, machineBanner, openSession, send, settled, step, toolCalls, waitReconnected } from './helpers';

// A link blip within the grace while the agent's command runs on devbox: the test script unplugs
// devbox's network (a silent partition: nothing is closed, only the SSH keepalives notice), the
// command finishes on devbox meanwhile, and the network comes back. The call only paused: the
// controller reconnects and resends it by its call id, devbox answers from its journal, and the
// agent gets the result once (the script checks the command ran once and the link-drop note).
test('a link blip mid-command pauses the call; the result arrives once', async ({ page }) => {
  await login(page, 'Laptop');
  await openSession(page, 'two');
  await send(page, 'Run the slow one');
  const slow = toolCalls(page, '# slow');
  await expect(slow).toHaveClass(/s-running/);
  await step('cut');

  // Detected by the keepalives; the session keeps working, only devbox's calls wait.
  await expect(machineBanner(page, 'devbox')).toContainText('unreachable', { timeout: 180_000 });
  await expect(header(page).getByLabel('Workspaces').locator('[data-workspace]').first()).toHaveClass(/\baway\b/);
  await expect(header(page).locator('.status')).toHaveText('Working');
  await expect(slow).toHaveClass(/s-running/);
  await step('restore');

  await waitReconnected(page, 'devbox');
  await expect(slow).toHaveClass(/s-completed/);
  await expect(lastReply(page)).toHaveText('Slow one done.');
  await settled(page);
});
