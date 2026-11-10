import { expect, type Page, test } from '@playwright/test';
import { header, login, newSession, requestCards, send, sessionRow, signIn, step, timeline } from './helpers';

// pi (on the server) runs `git push` in devbox's shell, which runs under the workspace's nono
// profile there: nono mediates the push and asks the host daemon, which raises an approval
// through the server. It shows as a sandbox card pinned above the composer and in the
// notifications panel of both tabs. One tab allows it once (the push lands); the other denies the next push with a
// reason (pi sees it; the test script checks the remote and what pi told the model).
// Push (push.spec.ts subscribed an iPhone, with no delay): nothing is pushed while a tab is in
// use, and the pending approval is pushed once both tabs are hidden (the script checks the mock
// push service).
test('git push approvals across two tabs', async ({ page, browser }) => {
  await login(page, 'Laptop');
  await newSession(page, { workspaces: [{ machine: 'devbox', path: '~/push' }], name: 'push', level: 'Strict', prompt: 'Push the branch' });
  const { context, page: other } = await signIn(browser, 'Desktop');
  await sessionRow(other, 'push').click();
  await other.locator('body').press(']');
  const notes = other.getByRole('complementary', { name: 'Notifications' });

  // The approval reaches both tabs and the notifications panel.
  const card = (p: Page) => requestCards(p, 'sandbox').last();
  for (const p of [page, other]) {
    await expect(card(p)).toHaveClass(/\bpending\b/);
    await expect(card(p).locator('pre.code')).toContainText('git push origin HEAD:allowed');
    await expect(card(p)).toContainText('remote origin · branch HEAD:allowed');
    await expect(header(p).locator('.status')).toHaveText('Needs approval');
  }
  const noted = notes.locator('.card.k-blocking', { hasText: 'push' });
  await expect(noted.locator('.rq-sandbox')).toContainText('HEAD:allowed');

  // Allowed once on the laptop: the desktop sees who answered, and the run finishes.
  await card(page).getByRole('button', { name: 'Allow once' }).click();
  for (const p of [page, other]) await expect(card(p).locator('.res')).toHaveText('→ Allow once · Laptop');
  await expect(noted).toHaveCount(0);
  await expect(timeline(page).locator('.msg.r-agent').last()).toHaveText('Pushed.');

  // Denied with a reason on the desktop.
  await send(other, 'Push again');
  for (const p of [page, other]) await expect(card(p).locator('pre.code')).toContainText('HEAD:denied');
  await expect(card(page)).toHaveClass(/\bpending\b/);
  await step('push-held');
  for (const p of [page, other]) await setHidden(p, true);
  await step('push-released');
  for (const p of [page, other]) await setHidden(p, false);
  await card(other).getByRole('button', { name: 'Deny with reason' }).click();
  await card(other).getByLabel('Reason').fill('not this branch');
  await card(other).getByLabel('Reason').press('Enter');
  for (const p of [page, other]) await expect(card(p).locator('.res')).toHaveText('→ Deny · Desktop');
  await expect(timeline(page).locator('.msg.r-agent').last()).toHaveText('The push was denied.');
  await context.close();
});

/** Makes a tab look hidden (or visible again): it reports that it isn't shown. */
async function setHidden(page: Page, hidden: boolean): Promise<void> {
  await page.evaluate((hidden) => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => (hidden ? 'hidden' : 'visible') });
    document.dispatchEvent(new Event('visibilitychange'));
  }, hidden);
}
