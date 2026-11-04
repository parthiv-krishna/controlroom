import { expect, test } from '@playwright/test';
import { lastReply, login, openSession, send, settled, step, toolCalls, waitReconnected } from './helpers';

// A real reboot of buildbox while the agent's command runs in its shell: the shell died with the
// host, so after the reconnect the call ends as lost (it may have partly run; it is never run
// again). The next command in that shell runs in a fresh shell in the last directory, without
// the old variables, and the agent is told the shell was reset (the test script checks).
test('a host reboot mid-command loses the call and resets the shell', async ({ page }) => {
  await login(page, 'Laptop');
  await openSession(page, 'two');
  await send(page, 'Prepare buildbox and wait there');
  await expect(toolCalls(page, '# prepare')).toHaveClass(/s-completed/);
  const waiting = toolCalls(page, '# wait');
  await expect(waiting).toHaveClass(/s-running/);
  await step('reboot');

  await waitReconnected(page, 'buildbox');
  await expect(waiting).toHaveClass(/s-failed/, { timeout: 120_000 });
  await expect(lastReply(page)).toHaveText('buildbox went away mid-command.');
  await settled(page);

  await send(page, 'Check the shell');
  await expect(toolCalls(page, '# check')).toHaveClass(/s-completed/);
  await expect(lastReply(page)).toHaveText('The shell was reset.');
  await settled(page);
});
