import { expect, test } from '@playwright/test';
import { closeSettings, env, header, login, machineCard, newSession, openMachines, send, step, timeline } from './helpers';

// buildbox runs an older daemon (the server bundled it). A run is held in flight at a model gate
// while the test script restarts the server with the current daemons: the machine shows the
// update but isn't upgraded while busy. Once the run ends it is upgraded, shows the new version,
// and the session lives on under the new daemon.
test('auto-upgrade waits for the run and keeps the session', async ({ page }) => {
  const version = env('E2E_VERSION');
  const escaped = version.replace(/\./g, '\\.');
  await login(page, 'Laptop');
  await newSession(page, { machine: 'buildbox', cwd: '~/work', name: 'build', level: 'Strict', prompt: 'Build it' });
  const reply = timeline(page).locator('.msg.r-agent').last();
  await expect(reply).toHaveText('Building');
  await step('restart');

  // The page reconnects to the restarted server.
  let settings = await openMachines(page);
  const card = machineCard(settings, 'buildbox');
  await expect(card.locator('.chip')).toHaveText('connected');
  await expect(card).toContainText(`update available (${version}-old → ${version})`);
  await closeSettings(page);
  await expect(header(page).locator('.status')).toHaveText('Working');
  await step('update-available');

  await expect(reply).toHaveText('Building done.');
  settings = await openMachines(page);
  await expect(card).toContainText(new RegExp(`controlroomd ${escaped} ·`), { timeout: 120_000 });
  await expect(card.locator('.chip')).toHaveText('connected');
  await expect(card).not.toContainText('update available');
  await step('upgraded');
  await closeSettings(page);

  await send(page, 'Are you still there?');
  await expect(timeline(page).locator('.msg.r-agent').last()).toHaveText('Still here.');
  await expect(header(page).locator('.status')).toHaveText('Complete');
});
