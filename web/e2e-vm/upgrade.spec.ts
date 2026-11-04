import { expect, test } from '@playwright/test';
import { closeSettings, env, header, lastReply, login, machineCard, newSession, openMachines, send, settled, step, toolCalls } from './helpers';

// buildbox runs an older host daemon (the server bundled it). The agent's command there is held
// (it waits for a file) while the test script restarts the web server with the current daemons:
// the call only pauses (the agent daemon keeps it and the new controller resends it), the
// machine shows the update but isn't upgraded while the call is in flight. Once it ends the
// daemon is upgraded, and the shell (a detached PTY supervisor) lives on with its state.
test('auto-upgrade waits for the host call and keeps the shell', async ({ page }) => {
  const version = env('E2E_VERSION');
  const escaped = version.replace(/\./g, '\\.');
  await login(page, 'Laptop');
  await newSession(page, { workspaces: [{ machine: 'buildbox', path: '~/build' }], name: 'build', level: 'Strict', prompt: 'Build it' });
  const build = toolCalls(page, /make-it/);
  await expect(build).toHaveClass(/s-running/);
  await step('restart');

  // The page reconnects to the restarted server; the call is still in flight.
  let settings = await openMachines(page);
  const card = machineCard(settings, 'buildbox');
  await expect(card.locator('.chip')).toHaveText('connected');
  await expect(card).toContainText(`update available (${version}-old → ${version})`);
  await closeSettings(page);
  await expect(header(page).locator('.status')).toHaveText('Working');
  await expect(build).toHaveClass(/s-running/);
  await step('update-available');

  await expect(build).toHaveClass(/s-completed/);
  await expect(lastReply(page)).toHaveText('Built.');
  settings = await openMachines(page);
  await expect(card).toContainText(new RegExp(`controlroomd ${escaped} ·`), { timeout: 120_000 });
  await expect(card.locator('.chip')).toHaveText('connected');
  await expect(card).not.toContainText('update available');
  await step('upgraded');
  await closeSettings(page);

  await send(page, 'Are you still there?');
  await expect(lastReply(page)).toHaveText('Still here.');
  await settled(page);
});
