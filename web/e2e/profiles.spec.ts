import { expect, test, type Page } from '@playwright/test';
import type { Profile } from '../src/lib/protocol';
import { M, P, S, calls, emitState, start } from './helpers';

// Profiles (docs/design/profiles-tenants.md §10) on the fixture build: `?profiles=3` has three
// (Orchard; Lantern, which shares devbox with Orchard; private Quill), the committed fixture one,
// `?profiles=0` none yet, and `?profiles=legacy` is a server before profiles.

const header = (page: Page) => page.locator('[data-pane="header"]');
const listTitle = (page: Page) => page.getByRole('region', { name: 'Sessions' }).locator('.lt');
const list = (page: Page) => page.getByRole('listbox', { name: 'Session list' });
const rail = (page: Page) => page.getByRole('navigation', { name: 'Projects' });

/** A profile as the server would answer `profiles.create` (queued before the dialog's Create). */
const created = (name: string): Profile => ({
  id: '01JAF00000000000000000000A',
  name,
  color: '#34d399',
  private: false,
  models: { kind: 'any' },
  model_defaults: { provider: null, model: null, thinking: null },
  position: 1,
  created_at: 1767600000000,
});
const replyCreate = (page: Page, p: Profile) => page.evaluate((p) => window.__fixture!.reply('profiles.create', p), p);

/** A screenshot once two in a row agree (the first load has settled). */
async function settledShot(page: Page): Promise<Buffer> {
  let last: Buffer = Buffer.alloc(0);
  await expect(async () => {
    const shot = await page.screenshot({ animations: 'disabled' });
    const same = shot.equals(last);
    last = shot;
    expect(same).toBe(true);
  }).toPass({ timeout: 15_000 });
  return last;
}

for (const phone of [false, true]) {
  test(`one profile looks exactly like a server before profiles (${phone ? 'phone' : 'desktop'})`, async ({ page }) => {
    if (phone) await page.setViewportSize({ width: 390, height: 844 });
    await start(page, undefined, 'paused&profiles=legacy');
    const before = await settledShot(page);
    for (const query of ['paused', 'paused&profiles=0']) {
      await start(page, undefined, query);
      expect((await settledShot(page)).equals(before), query).toBe(true);
      await expect(page.locator('.pchip, [data-profile]')).toHaveCount(0);
    }
    if (!phone) {
      await expect(rail(page).getByRole('button', { name: 'All projects' })).toBeVisible();
      await expect(rail(page).getByRole('button', { name: 'Unassigned' })).toBeVisible();
    }
  });
}

test('fresh install: adding a machine, starting a session and creating a project ask for the first profile', async ({ page }) => {
  await start(page, undefined, 'paused&profiles=0');
  // Settings shows the picker with only ＋.
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByRole('radiogroup', { name: 'Profile' }).getByRole('radio')).toHaveCount(0);
  await expect(settings.getByRole('button', { name: 'New profile' })).toBeVisible();

  // + Machine: the New profile dialog first, then the machine joins it.
  await settings.getByRole('button', { name: '+ Machine' }).click();
  const dialog = page.getByRole('dialog', { name: 'New profile' });
  await dialog.getByLabel('Name').fill('Garden club');
  await dialog.getByRole('radio', { name: '#f472b6' }).click();
  await dialog.getByLabel('Private').check();
  await replyCreate(page, created('Garden club'));
  await dialog.getByRole('button', { name: '↵ Create' }).click();
  await expect(dialog).toHaveCount(0);
  expect((await calls(page, 'profiles.create')).map((c) => c.params)).toEqual([{ name: 'Garden club', color: '#f472b6', private: true }]);
  await expect(settings.getByRole('heading', { name: 'Add machine' })).toBeVisible();
  await settings.getByLabel('Host').fill('devbox.example.com');
  await settings.getByLabel('User').fill('alice');
  await settings.getByRole('button', { name: 'Connect' }).click();
  await expect.poll(async () => (await calls(page, 'machines.add')).map((c) => c.params)).toEqual([
    expect.objectContaining({ host: 'devbox.example.com', user: 'alice', profile_ids: [created('').id] }),
  ]);
  await page.getByRole('button', { name: 'Close settings' }).click();

  // The feed has no profile yet (the fixture has no backend): a session asks again, then carries on in it.
  await page.getByRole('button', { name: 'New session', exact: true }).click();
  await replyCreate(page, created('Book club'));
  await page.getByRole('dialog', { name: 'New profile' }).getByLabel('Name').fill('Book club');
  await page.getByRole('dialog', { name: 'New profile' }).getByRole('button', { name: '↵ Create' }).click();
  const ns = page.getByRole('dialog', { name: 'New session' });
  await ns.getByLabel('Directory 1').fill('~/src/acme-api');
  await ns.getByRole('button', { name: '↵ Create' }).click();
  await expect.poll(async () => (await calls(page, 'session.create')).map((c) => c.params)).toEqual([expect.objectContaining({ profile_id: created('').id })]);

  // Once the profile is in the feed, nothing asks any more.
  await emitState(page, [{ kind: 'profile_upserted', profile: created('Book club') }]);
  await rail(page).getByRole('button', { name: 'New project' }).click();
  const project = page.getByRole('dialog', { name: 'Project' });
  await project.getByLabel('Name').fill('Seeds');
  await project.getByRole('button', { name: '↵ Save' }).click();
  expect((await calls(page, 'projects.create')).map((c) => c.params)).toEqual([expect.objectContaining({ name: 'Seeds', profile_id: created('').id })]);
  expect(await calls(page, 'profiles.create')).toHaveLength(2);
  // Still one profile: nothing about profiles shows in the views.
  await expect(page.locator('.pchip, [data-profile]')).toHaveCount(0);
});

test('three profiles: the rail groups, every view is scoped, and new sessions inherit the profile', async ({ page }) => {
  await start(page, undefined, 'paused&profiles=3');
  // Groups with thin labels; each has its ∗; no Unassigned.
  for (const name of ['Profile Orchard', 'Profile Lantern', 'Profile Quill (private)']) await expect(rail(page).getByRole('group', { name, exact: true })).toBeVisible();
  await expect(rail(page).getByRole('button', { name: 'Unassigned' })).toHaveCount(0);
  await expect(rail(page).getByRole('group', { name: 'Profile Lantern' }).getByRole('button')).toHaveText([/^∗\s*1$/, /^Do\s*1$/]);
  await expect(listTitle(page)).toHaveText('Orchard');
  await expect(list(page).getByText('auth token refresh race')).toBeVisible();
  await expect(list(page).getByText('blog: deep dive')).toHaveCount(0);

  // Entering Lantern: its sessions, its board; the open session follows.
  await rail(page).getByRole('button', { name: 'All Lantern' }).click();
  await expect(listTitle(page)).toHaveText('Lantern');
  await expect(list(page).getByText('blog: deep dive')).toBeVisible();
  await expect(list(page).getByText('auth token refresh race')).toHaveCount(0);
  await expect(header(page).getByRole('button', { name: 'Profile Lantern: all its sessions' })).toBeVisible();
  await page.locator('body').press('t');
  const board = page.getByRole('region', { name: 'Tasks' });
  await expect(board.getByText('Docs refresh')).toBeVisible();
  await expect(board.getByText('Fix random logouts')).toHaveCount(0);
  await page.locator('body').press('t');

  // New session: the profile first, then its machines only; it is sent along.
  await page.getByRole('button', { name: 'New session', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'New session' });
  await expect(dialog.getByLabel('Profile')).toContainText('Lantern');
  await dialog.getByLabel('Machine 1').click();
  await expect(page.getByRole('listbox', { name: 'Machine 1' }).getByRole('option')).toHaveText(['devbox']);
  await page.keyboard.press('Escape');
  await dialog.getByLabel('Profile').click();
  await page.getByRole('option', { name: /Quill/ }).click();
  await dialog.getByLabel('Machine 1').click();
  await expect(page.getByRole('listbox', { name: 'Machine 1' }).getByRole('option')).toHaveText([/armbox/, /oldbox/]);
  await page.keyboard.press('Escape');
  // The default level counts the profile's (Quill's is off; its machines set none).
  await expect(dialog.getByLabel('Sandbox level')).toContainText('Defaults (Off)');
  await dialog.getByLabel('Profile').click();
  await page.getByRole('option', { name: /Lantern/ }).click();
  await dialog.getByLabel('Directory 1').fill('~/notes');
  await dialog.getByRole('button', { name: '↵ Create' }).click();
  await expect.poll(async () => (await calls(page, 'session.create')).map((c) => c.params)).toEqual([
    expect.objectContaining({ profile_id: P.lantern, workspaces: [{ machine_id: M.devbox, path: '~/notes' }] }),
  ]);
});

test('three profiles: deep links, the palette and notifications cross profiles', async ({ page }) => {
  // A deep link to another profile's session enters it.
  await start(page, S.deploy, 'paused&profiles=3');
  await expect(header(page).locator('.et.heading')).toHaveText('legacy deploy scripts');
  await expect(listTitle(page)).toContainText('Quill');
  await expect(listTitle(page).getByRole('img', { name: 'private' })).toBeVisible();
  await expect(header(page).getByRole('button', { name: 'Profile Quill (private): all its sessions' })).toBeVisible();
  await page.evaluate((id) => (location.hash = `#/s/${id}`), S.blog);
  await expect(header(page).locator('.et.heading')).toHaveText('blog: deep dive');
  await expect(listTitle(page)).toHaveText('Lantern');

  // The palette lists every profile's sessions, the profile first in the hint.
  await page.locator('body').press('Control+Shift+K');
  const palette = page.getByRole('dialog', { name: 'Command palette' });
  await palette.getByLabel('Command').fill('inbox');
  await expect(palette.getByRole('option', { name: /inbox triage/ }).locator('.h')).toHaveText(['Quill', 'armbox', 'idle']);
  await page.keyboard.press('Enter');
  await expect(header(page).locator('.et.heading')).toHaveText('inbox triage');
  await expect(listTitle(page)).toContainText('Quill');
  await page.locator('body').press('Control+Shift+K');
  await palette.getByLabel('Command').fill(':profile orch');
  await page.keyboard.press('Enter');
  await expect(listTitle(page)).toHaveText('Orchard');

  // Notifications under ∗ aggregate every profile, each card tagged; a project stays scoped.
  await page.locator('body').press(']');
  const notes = page.getByRole('complementary', { name: 'Notifications' });
  await expect(notes.getByRole('img', { name: 'Profile Quill (private)' }).first()).toBeVisible();
  await expect(notes.getByRole('img', { name: 'Profile Lantern' }).first()).toBeVisible();
  await expect(notes.getByRole('img', { name: 'Profile Orchard' }).first()).toBeVisible();
  await rail(page).getByRole('button', { name: 'Acme API' }).click();
  await expect(notes.getByText('auth token refresh race').first()).toBeVisible();
  await expect(notes.getByRole('img', { name: /Profile (Quill|Lantern)/ })).toHaveCount(0);
  await rail(page).getByRole('button', { name: 'All Orchard' }).click();
  // Opening another profile's card enters it.
  await notes.locator('.card').filter({ hasText: 'docs site refresh' }).first().locator('.ch b').click();
  await expect(listTitle(page)).toHaveText('Lantern');
});

test('three profiles: Settings picker, shared machines, the model policy and the move dialog', async ({ page }) => {
  await start(page, undefined, 'paused&profiles=3');
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  const picker = settings.getByRole('radiogroup', { name: 'Profile' });
  await expect(picker.getByRole('radio')).toHaveCount(3);
  await expect(picker.getByRole('radio', { name: 'Profile Orchard' })).toHaveAttribute('aria-checked', 'true');
  await picker.getByRole('radio', { name: 'Profile Lantern' }).click();
  // Lantern's machines: devbox, shared with Orchard. SSH keys stay below, global.
  await expect(settings.locator('.card[data-machine]')).toHaveCount(1);
  await expect(settings.locator(`[data-machine="${M.devbox}"] [data-shared]`)).toContainText('Shared with');
  await expect(settings.locator(`[data-machine="${M.devbox}"] [data-shared]`).getByRole('img', { name: 'Profile Orchard' })).toBeVisible();
  await expect(settings.getByText('SSH keys')).toBeVisible();

  // Models: Lantern lists its accounts; unchecking one updates the policy.
  await settings.getByRole('tab', { name: 'Models' }).click();
  await expect(settings.getByLabel('Accounts in Lantern')).toContainText('Only the checked ones');
  await expect(settings.getByLabel('Use OpenAI in Lantern')).toBeChecked();
  await expect(settings.locator('[data-provider="anthropic"]')).toHaveClass(/\bdim\b/);
  await settings.getByLabel('Use OpenAI in Lantern').uncheck();
  await settings.getByLabel('Accounts in Lantern').click();
  await page.getByRole('option', { name: 'Any' }).click();
  await expect.poll(async () => (await calls(page, 'profiles.update')).map((c) => c.params)).toEqual([
    expect.objectContaining({ profile_id: P.lantern, models: { kind: 'only', accounts: ['openrouter'] } }),
    expect.objectContaining({ profile_id: P.lantern, models: { kind: 'any' } }),
  ]);
  await expect(settings.getByText('Defaults in Lantern')).toBeVisible();
  await settings.locator('[data-defaults]').getByLabel('Default provider').click();
  await expect(page.getByRole('option', { name: /Anthropic/ })).toHaveCount(0);
  await page.keyboard.press('Escape');
  // A private profile is never Any.
  await picker.getByRole('radio', { name: 'Profile Quill (private)' }).click();
  await settings.getByLabel('Accounts in Quill').click();
  await expect(page.getByRole('option', { name: /^Any/ })).toBeDisabled();
  await page.keyboard.press('Escape');

  // Sandbox: the profile's own card, above its machines.
  await settings.getByRole('tab', { name: 'Sandbox' }).click();
  await expect(settings.locator(`[data-scope="f:${P.quill}"]`).getByLabel('Sandbox level for profile Quill')).toContainText('Off');
  await expect(settings.locator('[data-scope^="m:"]').first().getByRole('button', { name: /^Sandbox level for/ })).toContainText('Quill default (off)');
  // Notifications and General have no picker.
  await settings.getByRole('tab', { name: 'General' }).click();
  await expect(picker).toHaveCount(0);
  await expect(settings.getByLabel('Idle stop')).toBeVisible();
  await page.getByRole('button', { name: 'Close settings' }).click();

  // Move a project with its tasks: the dialog previews, warns that live agents restart, then moves.
  await rail(page).getByRole('button', { name: 'Docs' }).click();
  await page.getByRole('button', { name: 'Edit project' }).click();
  await page.getByRole('dialog', { name: 'Project' }).getByRole('button', { name: 'Move to profile…' }).click();
  const move = page.getByRole('dialog', { name: 'Move to profile' });
  await expect(move.getByLabel('From profile')).toContainText('Lantern');
  await expect(move.getByLabel('To profile')).toContainText('Orchard');
  await expect(move.getByRole('checkbox', { name: 'Docs', exact: true })).toBeChecked();
  const preview = move.getByRole('region', { name: 'What moves' });
  await expect(preview).toContainText('Docs refresh');
  await expect(preview.getByRole('alert')).toContainText('Live agents restart: docs site refresh, blog: deep dive');
  await expect(preview).toContainText('Left behind NixOS module hardening');
  await move.getByRole('button', { name: '↵ Move' }).click();
  await expect(move).toHaveCount(0);
  expect((await calls(page, 'profiles.move')).map((c) => c.params)).toEqual([
    { profile_id: P.orchard, machine_ids: [], project_ids: ['01JAP000000000000000000003'], session_ids: [], share: false, dry_run: true },
    { profile_id: P.orchard, machine_ids: [], project_ids: ['01JAP000000000000000000003'], session_ids: [], share: false },
  ]);
});

test('three profiles on the phone: the drawer groups by profile, the bell counts every profile', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await start(page, undefined, 'paused&profiles=3');
  const switcher = page.getByRole('button', { name: 'Projects' });
  await expect(switcher).toContainText('Orchard');
  const bell = page.getByRole('button', { name: 'Notifications' });
  const total = await bell.innerText();
  await switcher.click();
  const drawer = page.getByRole('dialog', { name: 'Projects' });
  for (const name of ['Profile Orchard', 'Profile Lantern', 'Profile Quill (private)']) await expect(drawer.getByRole('group', { name, exact: true })).toBeVisible();
  await expect(drawer.getByRole('button', { name: /Unassigned/ })).toHaveCount(0);
  await drawer.getByRole('button', { name: /All Quill/ }).click();
  await expect(drawer).toHaveCount(0);
  await expect(switcher).toContainText('Quill');
  await expect(switcher.getByRole('img', { name: 'private' })).toBeVisible();
  await expect(list(page).getByText('inbox triage')).toBeVisible();
  await expect(list(page).getByText('auth token refresh race')).toHaveCount(0);
  await expect(bell).toHaveText(total);
  await switcher.click();
  await drawer.getByRole('button', { name: /^Do Docs/ }).click();
  await expect(switcher).toContainText('Docs');
  await expect(list(page).getByText('blog: deep dive')).toBeVisible();
});

test('profiles take any name: long, unicode, and two alike', async ({ page }) => {
  await start(page, undefined, 'paused&profiles=3');
  const long = 'Ünïcødé 工作 🌱 with a name that goes on well past the rail and every chip';
  const renamed = (id: string, name: string, extra: Partial<Profile> = {}): Profile => ({ ...created(name), id, ...extra });
  await emitState(page, [
    { kind: 'profile_upserted', profile: renamed(P.orchard, 'Work', { position: 1 }) },
    { kind: 'profile_upserted', profile: renamed(P.lantern, 'Work stuff', { position: 2 }) },
    { kind: 'profile_upserted', profile: renamed(P.quill, long, { position: 3, private: true }) },
  ]);
  await expect(rail(page).getByRole('button', { name: 'All Work', exact: true })).toBeVisible();
  await expect(rail(page).getByRole('button', { name: 'All Work stuff', exact: true })).toBeVisible();
  // The long label stays inside the rail.
  const label = rail(page).getByRole('group', { name: `Profile ${long} (private)` }).locator('.gl');
  const railBox = (await rail(page).boundingBox())!;
  const labelBox = (await label.boundingBox())!;
  expect(labelBox.x + labelBox.width).toBeLessThanOrEqual(railBox.x + railBox.width);
  // `:profile work` is the exact match, not its look-alike.
  await rail(page).getByRole('button', { name: 'All Work stuff', exact: true }).click();
  await expect(listTitle(page)).toHaveText('Work stuff');
  await page.locator('body').press('Control+Shift+K');
  await page.getByRole('dialog', { name: 'Command palette' }).getByLabel('Command').fill(':profile work');
  await page.keyboard.press('Enter');
  await expect(listTitle(page)).toHaveText('Work');
  // A deep link into the long-named profile.
  await page.evaluate((id) => (location.hash = `#/s/${id}`), S.deploy);
  await expect(listTitle(page)).toContainText(long);
  await expect(header(page).getByRole('button', { name: `Profile ${long} (private): all its sessions` })).toBeVisible();
  await page.getByRole('button', { name: 'Settings' }).click();
  const picker = page.getByRole('dialog', { name: 'Settings' }).getByRole('radiogroup', { name: 'Profile' });
  await expect(picker.getByRole('radio')).toHaveText(['Work', 'Work stuff', long]);
});
