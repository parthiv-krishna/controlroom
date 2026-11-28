import { expect, test, type Locator, type Page } from '@playwright/test';
import fixture from '../fixtures/controlroom.json' with { type: 'json' };
import type { FeedSession, Workspace } from '../src/lib/protocol';
import { M, S, calls, emitState, start } from './helpers';

// A session's workspaces in its header: one is a `machine:path` pill as ever; several collapse
// into one chip ("N workspaces", a status dot each, ⚠ when shared) whose popover lists them and
// removes all but the primary. ⋯ › + Workspace adds one (machines of the session's profile only).

// SAFETY: JSON imports widen string unions; fixture.test.ts checks the fixture's consistency.
const session = (id: string) => fixture.snapshot.sessions.find((s) => s.id === id) as unknown as FeedSession;
const header = (page: Page) => page.locator('[data-pane="header"]');
const workspaces = (page: Page) => header(page).getByLabel('Workspaces', { exact: true });
const chip = (page: Page) => workspaces(page).getByRole('button', { name: /^\d+ workspaces/ });
const popover = (page: Page) => page.getByRole('dialog', { name: 'Session workspaces' });

/** A workspace as the server reports it. */
const workspace = (id: string, machine: string, path: string, state: 'connected' | 'unreachable' | 'connecting'): Workspace => ({
  id,
  machine_id: machine,
  path,
  root: path.replace('~', '/home/alice'),
  primary: false,
  added_at: 1767600000000,
  availability: { connectivity: { state }, since: 1767600000000 },
});

/** The server's update of a session's workspaces. */
const setWorkspaces = (page: Page, id: string, ws: Workspace[]) => emitState(page, [{ kind: 'session_upserted', session: { ...session(id), workspaces: ws } }]);

test('one workspace looks as before: the pill with its overlap warning, no chip', async ({ page }) => {
  await start(page, S.release);
  await expect(workspaces(page).locator('[data-workspace]')).toHaveText(['devbox:~/src/controlroom']);
  await expect(workspaces(page).locator('[data-workspace] .dot')).toHaveClass(/\bconnected\b/);
  await expect(chip(page)).toHaveCount(0);
  const overlap = workspaces(page).locator('[data-overlap]');
  await expect(overlap).toHaveAttribute('title', 'devbox:~/src/controlroom is shared with lint cleanup: edits can collide');
  await overlap.getByRole('button', { name: 'Shared with lint cleanup' }).click();
  await overlap.getByRole('dialog', { name: 'Shared directory' }).getByRole('button', { name: 'lint cleanup' }).click();
  await expect(page.getByRole('option', { name: /lint cleanup/ })).toHaveAttribute('aria-selected', 'true');
});

test('several workspaces collapse into one chip: a dot per workspace, ⚠ when shared', async ({ page }) => {
  // Two connected, one shared with another session.
  await start(page, S.auth);
  await expect(workspaces(page).locator('.ws')).toHaveCount(0);
  await expect(chip(page)).toHaveAccessibleName('2 workspaces, 1 shared with another session');
  await expect(chip(page)).toContainText('2 workspaces');
  await expect(chip(page)).toContainText('⚠');
  await expect(chip(page).locator('.dot')).toHaveClass([/\bconnected\b/, /\bconnected\b/]);
  await expect(chip(page)).toHaveAttribute('title', /buildbox:~\/src\/acme-api is shared with upgrade tokio to 1.47/);

  // The primary on an unreachable machine, the other connected: no ⚠.
  await start(page, S.dedupe);
  await expect(chip(page)).toHaveAccessibleName('2 workspaces, 1 unavailable');
  await expect(chip(page)).not.toContainText('⚠');
  await expect(chip(page).locator('.dot')).toHaveClass([/\bunreachable\b/, /\bconnected\b/]);

  // A third, on a machine still connecting.
  const [dedupe, samples] = session(S.dedupe).workspaces;
  await setWorkspaces(page, S.dedupe, [dedupe, samples, workspace('01JAW0000000000000000000A1', M.buildbox, '~/data/cache', 'connecting')]);
  await expect(chip(page)).toHaveAccessibleName('3 workspaces, 2 unavailable');
  await expect(chip(page).locator('.dot')).toHaveClass([/\bunreachable\b/, /\bconnected\b/, /\bconnecting\b/]);
});

test('the popover lists the workspaces, removes one after a confirm, never the primary', async ({ page }) => {
  await start(page, S.auth);
  await chip(page).click();
  const rows = popover(page).locator('[data-workspace]');
  await expect(rows).toHaveCount(2);
  const [primary, web] = [rows.nth(0), rows.nth(1)];
  await expect(primary).toContainText('buildbox:~/src/acme-api');
  await expect(primary).toContainText('primary');
  await expect(primary).toContainText('connected');
  await expect(web).toContainText('devbox:~/src/acme-web');
  await expect(web).not.toContainText('primary');
  // The primary can't be removed.
  await expect(primary.getByRole('button', { name: /^Remove/ })).toHaveCount(0);
  // The overlap, with a link to the other session.
  await expect(primary.locator('[data-overlap]')).toContainText('Shared with upgrade tokio to 1.47: edits can collide.');
  await expect(web.locator('[data-overlap]')).toHaveCount(0);

  // Remove asks first; Cancel keeps it.
  await web.getByRole('button', { name: 'Remove devbox:~/src/acme-web' }).click();
  const confirm = web.getByRole('group', { name: 'Confirm removal' });
  await expect(confirm).toContainText('Remove devbox:~/src/acme-web? Its shells end.');
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  await expect(confirm).toHaveCount(0);
  expect(await calls(page, 'session.workspace_remove')).toHaveLength(0);

  // A command still running there: refused, shown on the row.
  await page.evaluate(() => window.__fixture!.fail('session.workspace_remove', 'busy', { kind: 'busy' }));
  await web.getByRole('button', { name: 'Remove devbox:~/src/acme-web' }).click();
  await confirm.getByRole('button', { name: 'Remove' }).click();
  await expect(web.getByRole('alert')).toHaveText('A command or job is still running there');

  // Then it goes; with one workspace left the header shows its pill again.
  await confirm.getByRole('button', { name: 'Remove' }).click();
  const wsId = session(S.auth).workspaces[1].id;
  await expect.poll(async () => (await calls(page, 'session.workspace_remove')).map((c) => c.params)).toEqual([
    expect.objectContaining({ id: S.auth, workspace_id: wsId }),
    expect.objectContaining({ id: S.auth, workspace_id: wsId }),
  ]);
  await setWorkspaces(page, S.auth, [session(S.auth).workspaces[0]]);
  await expect(popover(page)).toHaveCount(0);
  await expect(chip(page)).toHaveCount(0);
  await expect(workspaces(page).locator('[data-workspace]')).toHaveText(['buildbox:~/src/acme-api']);
});

test('the popover links the session sharing a directory', async ({ page }) => {
  await start(page, S.auth);
  await chip(page).click();
  await popover(page).locator('[data-overlap]').getByRole('button', { name: 'upgrade tokio to 1.47' }).click();
  await expect(popover(page)).toHaveCount(0);
  await expect(page.getByRole('option', { name: /upgrade tokio to 1.47/ })).toHaveAttribute('aria-selected', 'true');
});

test('⋯ › + Workspace adds one on a machine of the session’s profile; a refusal shows inline', async ({ page }) => {
  // "blog: deep dive" is in Lantern, which has devbox only.
  await start(page, S.blog, 'paused&profiles=3');
  await header(page).getByRole('button', { name: 'Session actions' }).click();
  await header(page).getByRole('menu').getByRole('menuitem', { name: 'Add workspace' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add workspace' });
  await dialog.getByRole('button', { name: 'Machine', exact: true }).click();
  await expect(dialog.getByRole('listbox', { name: 'Machine', exact: true }).getByRole('option')).toHaveText(['devbox']);
  await dialog.getByRole('listbox', { name: 'Machine', exact: true }).getByRole('option', { name: 'devbox' }).click();
  // Already the session's: not sent.
  await dialog.getByLabel('Directory').fill('~/src/docs/blog');
  await dialog.getByRole('button', { name: /Add$/ }).click();
  await expect(dialog.getByRole('alert')).toHaveText('Already a workspace of this session');

  // devbox left the profile meanwhile: the server refuses, the dialog says why and stays.
  await page.evaluate(() =>
    window.__fixture!.fail('session.workspace_add', 'policy denied', { kind: 'policy_denied', profile_id: '01JAF000000000000000000002', reason: 'Not available in Lantern' }),
  );
  await dialog.getByLabel('Directory').fill('~/src/docs/drafts');
  await dialog.getByRole('button', { name: /Add$/ }).click();
  await expect(dialog.getByRole('alert')).toHaveText('Not available in Lantern');

  // An unreachable host, then a bad path, likewise.
  await page.evaluate((m) => window.__fixture!.fail('session.workspace_add', 'unavailable', { kind: 'machine_unavailable', machine_id: m, connectivity: { state: 'unreachable' } }), M.devbox);
  await dialog.getByRole('button', { name: /Add$/ }).click();
  await expect(dialog.getByRole('alert')).toHaveText("devbox can't be reached (unreachable)");
  await page.evaluate(() => window.__fixture!.fail('session.workspace_add', 'fs', { kind: 'fs_error', path: '~/src/docs/drafts', detail: 'No such file or directory' }));
  await dialog.getByRole('button', { name: /Add$/ }).click();
  await expect(dialog.getByRole('alert')).toHaveText('~/src/docs/drafts: No such file or directory');

  // Accepted: the dialog closes, and the header collapses once the feed has it.
  const added = workspace('01JAW0000000000000000000A2', M.devbox, '~/src/docs/drafts', 'connected');
  await page.evaluate((w) => window.__fixture!.reply('session.workspace_add', w), added);
  await dialog.getByRole('button', { name: /Add$/ }).click();
  await expect(dialog).toHaveCount(0);
  expect((await calls(page, 'session.workspace_add')).map((c) => c.params)).toEqual(
    Array(4).fill(expect.objectContaining({ id: S.blog, workspace: { machine_id: M.devbox, path: '~/src/docs/drafts' } })),
  );
  await setWorkspaces(page, S.blog, [...session(S.blog).workspaces, added]);
  await expect(chip(page)).toHaveAccessibleName('2 workspaces');
});

/**
 * Opens ⋯ › + Workspace on "blog: deep dive" (Lantern: devbox only), with devbox picked. Picking
 * it takes the focus from the directory field; a test's next `fill` gives it back at once, and the
 * list must stay open.
 */
async function addWorkspaceDialog(page: Page) {
  await start(page, S.blog, 'paused&profiles=3');
  await header(page).getByRole('button', { name: 'Session actions' }).click();
  await header(page).getByRole('menu').getByRole('menuitem', { name: 'Add workspace' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add workspace' });
  await dialog.getByRole('button', { name: 'Machine', exact: true }).click();
  await dialog.getByRole('listbox', { name: 'Machine', exact: true }).getByRole('option', { name: 'devbox' }).click();
  return dialog;
}

/** Every directory has `drafts-0` … `drafts-11`: enough for the list's full height, on every refresh. */
const manyDirs = (page: Page) =>
  page.evaluate(() =>
    window.__fixture!.respond('fs.list', ({ path }) => ({
      path,
      entries: Array.from({ length: 12 }, (_, i) => ({ name: `drafts-${i}`, kind: 'dir' as const, size: null, mtime: null })),
    })),
  );

type Box = { x: number; y: number; width: number; height: number };
const overlaps = (a: Box, b: Box) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
async function box(l: Locator): Promise<Box> {
  const b = await l.boundingBox();
  if (!b) throw new Error('not rendered');
  return b;
}

test('the directory suggestions never cover the Add workspace dialog’s buttons: Add works while they are open', async ({ page }) => {
  const dialog = await addWorkspaceDialog(page);
  await manyDirs(page);
  await dialog.getByLabel('Directory').fill('~/src/docs/dra');
  const list = dialog.getByRole('listbox', { name: 'Directories' });
  await expect(list.getByRole('option')).toHaveCount(12);
  // Fully in the window, and clear of both buttons.
  const l = await box(list);
  expect(l.y).toBeGreaterThanOrEqual(0);
  expect(l.y + l.height).toBeLessThanOrEqual(page.viewportSize()!.height);
  for (const name of [/Cancel$/, /Add$/]) expect(overlaps(l, await box(dialog.getByRole('button', { name })))).toBe(false);
  // A plain click on Add, with the list still open, lands on the button.
  const added = workspace('01JAW0000000000000000000A3', M.devbox, '~/src/docs/dra', 'connected');
  await page.evaluate((w) => window.__fixture!.reply('session.workspace_add', w), added);
  await expect(list).toBeVisible();
  await dialog.getByRole('button', { name: /Add$/ }).click({ timeout: 5_000 });
  await expect(dialog).toHaveCount(0);
  expect((await calls(page, 'session.workspace_add')).map((c) => c.params)).toEqual([
    expect.objectContaining({ id: S.blog, workspace: { machine_id: M.devbox, path: '~/src/docs/dra' } }),
  ]);
});

test('in a short window the directory suggestions shrink to stay fully visible', async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 420 });
  const dialog = await addWorkspaceDialog(page);
  await manyDirs(page);
  await dialog.getByLabel('Directory').fill('~/src/docs/dra');
  const list = dialog.getByRole('listbox', { name: 'Directories' });
  await expect(list.getByRole('option')).toHaveCount(12);
  const l = await box(list);
  expect(l.y).toBeGreaterThanOrEqual(0);
  expect(l.height).toBeLessThan(260);
  expect(overlaps(l, await box(dialog.getByRole('button', { name: /Add$/ })))).toBe(false);
});

test('the directory field’s keys: arrows pick, Tab drills in, Escape closes the list, Enter adds', async ({ page }) => {
  const dialog = await addWorkspaceDialog(page);
  await manyDirs(page);
  const field = dialog.getByLabel('Directory');
  const list = dialog.getByRole('listbox', { name: 'Directories' });
  await field.fill('~/src/docs/dra');
  await expect(list.getByRole('option')).toHaveCount(12);
  await field.press('ArrowDown');
  await expect(list.getByRole('option', { selected: true })).toHaveText(/^~\/src\/docs\/drafts-1\//);
  await field.press('Tab');
  await expect(field).toHaveValue('~/src/docs/drafts-1/');
  await expect(field).toBeFocused();
  await expect(list.getByRole('option').first()).toHaveText(/^~\/src\/docs\/drafts-1\/drafts-0\//);
  // Escape closes the list only; Enter picks the highlighted entry, then adds.
  await field.press('Escape');
  await expect(list).toHaveCount(0);
  await expect(dialog).toBeVisible();
  await field.press('ArrowDown');
  await expect(list.getByRole('option', { selected: true })).toHaveText(/^~\/src\/docs\/drafts-1\/drafts-1\//);
  await field.press('Enter');
  await expect(field).toHaveValue('~/src/docs/drafts-1/drafts-1');
  await expect(list).toHaveCount(0);
  const added = workspace('01JAW0000000000000000000A4', M.devbox, '~/src/docs/drafts-1/drafts-1', 'connected');
  await page.evaluate((w) => window.__fixture!.reply('session.workspace_add', w), added);
  await field.press('Enter');
  await expect(dialog).toHaveCount(0);
});

test('in the new-session dialog the last of several workspaces’ suggestions stay clear of Create', async ({ page }) => {
  await start(page);
  await page.getByRole('button', { name: 'New session', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'New session' });
  for (let i = 0; i < 3; i++) await dialog.getByRole('button', { name: '+ Workspace' }).click();
  await manyDirs(page);
  const field = dialog.getByLabel('Directory 4');
  await field.fill('~/src/docs/dra');
  const list = dialog.getByRole('listbox', { name: 'Directories' });
  await expect(list.getByRole('option')).toHaveCount(12);
  // Below the field, as in any form; clear of the dialog's buttons.
  const l = await box(list);
  expect(l.y).toBeGreaterThan((await box(field)).y);
  for (const name of [/Cancel$/, /Create$/]) expect(overlaps(l, await box(dialog.getByRole('button', { name })))).toBe(false);
});

test('⋯ has no + Workspace on an archived session', async ({ page }) => {
  await start(page, '01JAS000000000000000000016');
  await header(page).getByRole('button', { name: 'Session actions' }).click();
  await expect(header(page).getByRole('menu').getByRole('menuitem', { name: 'Unarchive' })).toBeVisible();
  await expect(header(page).getByRole('menu').getByRole('menuitem', { name: 'Add workspace' })).toHaveCount(0);
});

test('phone: the same chip; the list is a bottom sheet', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await start(page);
  await page.getByRole('listbox', { name: 'Session list' }).getByRole('option', { name: /dataset dedupe/ }).click();
  await expect(chip(page)).toHaveAccessibleName('2 workspaces, 1 unavailable');
  await chip(page).click();
  const sheet = popover(page);
  await expect(sheet.locator('[data-workspace]')).toHaveCount(2);
  await expect(sheet.locator('[data-workspace]').first()).toContainText(/unreachable since/);
  // Docked to the bottom, full width.
  const box = (await sheet.boundingBox())!;
  expect(box.x).toBe(0);
  expect(box.width).toBe(390);
  expect(Math.round(box.y + box.height)).toBe(844);
  await sheet.getByRole('button', { name: 'Remove devbox:~/data/samples' }).click();
  await expect(sheet.getByRole('group', { name: 'Confirm removal' })).toContainText('Remove devbox:~/data/samples? Its shells end.');
});
