import { expect, test } from '@playwright/test';
import {
  choose,
  closeSettings,
  login,
  machineBanner,
  machineCard,
  openMachines,
  openSession,
  send,
  settled,
  step,
  timeline,
  toolCalls,
  waitReconnected,
} from './helpers';

// An outage longer than devbox's link-loss grace (set to 1 min here), with a background job and
// a command running there. The command's call fails when the grace runs out ("devbox
// unreachable"), and the agent ends its turn. Meanwhile the job and the command finish on devbox.
// Once the network is back, the command's late result is injected, and the job's exit wakes the
// agent exactly once (the test script checks what the model saw).
test('past the grace a call fails; its late result and the job exit arrive once', async ({ page }) => {
  await login(page, 'Laptop');
  let settings = await openMachines(page);
  await choose(machineCard(settings, 'devbox'), 'Link-loss grace', '1 min');
  await closeSettings(page);

  await openSession(page, 'two');
  await send(page, 'Build in the background, then wait on devbox');
  await expect(toolCalls(page, /^job$/)).toHaveClass(/s-completed/);
  const late = toolCalls(page, '# late');
  await expect(late).toHaveClass(/s-running/);
  await step('cut');

  await expect(machineBanner(page, 'devbox')).toContainText('unreachable', { timeout: 180_000 });
  // The grace runs out: the call fails, and the agent is told why (and, in a notice, that its
  // jobs keep running there).
  await expect(late).toHaveClass(/s-failed/, { timeout: 180_000 });
  await expect(timeline(page).locator('.msg.r-agent', { hasText: "devbox is away; I'll wait." })).toBeVisible();
  await settled(page);
  await step('restore');

  await waitReconnected(page, 'devbox');
  await step('woken');
  await settled(page);

  settings = await openMachines(page);
  await choose(machineCard(settings, 'devbox'), 'Link-loss grace', '10 min');
  await closeSettings(page);
});
