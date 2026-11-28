import { expect, test, type Page } from '@playwright/test';
import fixture from '../fixtures/controlroom.json' with { type: 'json' };
import type { FeedSession } from '../src/lib/protocol';
import { S, calls, emitState, start } from './helpers';

// Restarting a session's agent (⋯ › Restart agent; a running turn stops after a one-line
// confirm), and the stale-credential hint that offers it (the fixture's state doesn't change; the
// calls are checked).
const header = (page: Page) => page.locator('[data-pane="header"]');
const restarts = async (page: Page) => (await calls(page, 'session.restart')).map((c) => (c.params as { id: string }).id);
// SAFETY: JSON imports widen string unions; fixture.test.ts checks the fixture's consistency.
const session = (id: string) => fixture.snapshot.sessions.find((s) => s.id === id) as unknown as FeedSession;

async function restartFromMenu(page: Page) {
  await header(page).getByRole('button', { name: 'Session actions' }).click();
  await header(page).getByRole('menu').getByRole('menuitem', { name: 'Restart agent' }).click();
}

test('⋯ › Restart agent: at once when idle, after a confirm while a turn runs', async ({ page }) => {
  await start(page, S.blog);
  await restartFromMenu(page);
  await expect.poll(() => restarts(page)).toEqual([S.blog]);
  await expect(page.getByRole('alertdialog')).toHaveCount(0);

  // A running session (the page stays: the calls so far are kept).
  await page.getByRole('option', { name: /flaky e2e tests/ }).click();
  await restartFromMenu(page);
  const confirm = page.getByRole('alertdialog', { name: 'Stop the turn and restart the agent?' });
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  expect(await restarts(page)).toEqual([S.blog]);
  await restartFromMenu(page);
  await confirm.getByRole('button', { name: 'Restart' }).click();
  await expect.poll(() => restarts(page)).toEqual([S.blog, S.flaky]);
});

test('stale credentials: a pinned hint with Restart agent', async ({ page }) => {
  await start(page, S.blog);
  const hint = page.getByRole('status').filter({ hasText: 'Credentials changed since this agent started.' });
  await expect(hint).toHaveCount(0);
  await emitState(page, [{ kind: 'session_upserted', session: { ...session(S.blog), stale_credentials: true } }]);
  await expect(hint).toBeVisible();
  await hint.getByRole('button', { name: 'Restart agent' }).click();
  await expect.poll(() => restarts(page)).toEqual([S.blog]);
  await emitState(page, [{ kind: 'session_upserted', session: { ...session(S.blog), stale_credentials: false } }]);
  await expect(hint).toHaveCount(0);
});
