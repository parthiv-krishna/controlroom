import { expect, test, type Page } from '@playwright/test';
import { M, S, calls, start } from './helpers';

// Settings › Sandbox path grants and never-paths, and "Always allow" / "Never" on a file request.

async function openSandbox(page: Page) {
  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Sandbox' }).click();
  return settings;
}

const changes = async (page: Page) => (await calls(page, 'sandbox.set_profile')).map((c) => (c.params as { change: unknown }).change);

test('sandbox tab: path grants with access, a broad-grant confirm, and never-paths', async ({ page }) => {
  await start(page);
  const settings = await openSandbox(page);
  const devbox = settings.locator(`[data-profile="m:${M.devbox}"]`);
  const grant = devbox.getByLabel('Add path grant for devbox');

  // Relative paths and .. are refused before anything is sent.
  await grant.fill('src/cache');
  await grant.press('Enter');
  await expect(devbox.getByRole('alert')).toContainText('absolute path');
  await grant.fill('~/a/../b');
  await grant.press('Enter');
  await expect(devbox.getByRole('alert')).toContainText('..');

  // A read-write grant, normalized.
  await devbox.getByRole('button', { name: 'Access for devbox' }).click();
  await page.getByRole('option', { name: 'read-write' }).click();
  await grant.fill('~/.cache//nix/');
  await grant.press('Enter');
  await expect(devbox.getByRole('alert')).toHaveCount(0);
  await expect.poll(() => changes(page)).toEqual([{ kind: 'add_grant', grant: { kind: 'path', value: '~/.cache/nix', access: 'read_write' } }]);

  // `/` read-write is refused; a broad grant needs a confirmation.
  await grant.fill('/');
  await grant.press('Enter');
  await expect(devbox.getByRole('alert')).toContainText('can’t be granted read-write');
  await devbox.getByRole('button', { name: 'Access for devbox' }).click();
  await page.getByRole('option', { name: 'read', exact: true }).click();
  await grant.fill('~');
  await grant.press('Enter');
  const confirm = page.getByRole('alertdialog', { name: /Grant read access to ~ on devbox/ });
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  expect(await changes(page)).toHaveLength(1);
  await grant.fill('/home');
  await grant.press('Enter');
  await page.getByRole('alertdialog', { name: /Grant read access to \/home on devbox/ }).getByRole('button', { name: 'Grant' }).click();
  await expect.poll(async () => (await changes(page)).at(-1)).toEqual({ kind: 'add_grant', grant: { kind: 'path', value: '/home', access: 'read' } });

  // Never-paths: listed, added and removed.
  await expect(devbox.locator('[data-never="~/.bash_history"]')).toBeVisible();
  const never = devbox.getByLabel('Add never-path for devbox');
  await never.fill('~/.lesshst');
  await never.press('Enter');
  await expect.poll(async () => (await changes(page)).at(-1)).toEqual({ kind: 'add_never', path: '~/.lesshst' });
  await devbox.getByRole('button', { name: 'Remove never-path ~/.bash_history on devbox' }).click();
  await expect.poll(async () => (await changes(page)).at(-1)).toEqual({ kind: 'remove_never', path: '~/.bash_history' });

  // Removing a grant (buildbox has one).
  const buildbox = settings.locator(`[data-profile="m:${M.buildbox}"]`);
  await buildbox.getByRole('button', { name: 'Remove grant /home/alice/.cache/cargo on buildbox' }).click();
  await expect.poll(async () => (await changes(page)).at(-1)).toMatchObject({ kind: 'remove_grant', grant_id: '01JAG000000000000000000021' });
});

test('a file request: access, path, where; terse options with full names; answer lines', async ({ page }) => {
  await start(page, S.deploy);
  const card = page.getByRole('main').locator('[data-request="req-path-1"]').first();
  await expect(card).toContainText('read-write');
  await expect(card).toContainText('/home/alice/.cache/nix/fetcher-cache-v4.sqlite-shm');
  await expect(card.locator('[data-where]')).toHaveText(/outside the workspace · on devbox/);
  const always = card.getByRole('button', { name: 'Always allow read-write access to /home/alice/.cache/nix/fetcher-cache-v4.sqlite-shm on devbox, in new sessions too' });
  await expect(always).toHaveText('Always allow (devbox)');
  await expect(card.getByRole('button', { name: /^Never allow .* on devbox/ })).toHaveText('Never (devbox)');
  // "Deny…" with a reason denies; it is never "Never".
  await expect(card.getByRole('button', { name: 'Deny with reason' })).toHaveText('Deny…');
  await always.click();
  await expect.poll(async () => (await calls(page, 'request.respond')).at(-1)?.params).toMatchObject({
    id: S.deploy,
    request_id: 'req-path-1',
    response: { kind: 'choice', option_id: 'allow-always' },
  });
  await card.getByRole('button', { name: /^Never allow/ }).click();
  await expect.poll(async () => (await calls(page, 'request.respond')).at(-1)?.params).toMatchObject({ response: { kind: 'choice', option_id: 'never' } });

  // The timeline's answered file requests name the machine.
  const timeline = page.getByRole('main');
  await expect(timeline.locator('[data-request="f.always"]')).toContainText('→ Always allow (devbox)');
  await expect(timeline.locator('[data-request="f.never"]')).toContainText('→ Never (devbox)');
});

test('sandbox: phone width', async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  await start(page);
  await page.getByRole('button', { name: 'Projects' }).click();
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Sandbox' }).click();
  await expect(settings.getByLabel('Add path grant for devbox')).toBeVisible();
  // Nothing wider than the screen.
  const overflow = await settings.evaluate((el) =>
    [...el.querySelectorAll('[data-profile] input, [data-profile] button, [data-profile] .chip-s')].some((e) => e.getBoundingClientRect().right > window.innerWidth + 1),
  );
  expect(overflow).toBe(false);

  await page.keyboard.press('Escape');
  await expect(settings).toHaveCount(0);
  await page.goto(`/?paused#/s/${S.deploy}`);
  const card = page.getByRole('main').locator('[data-request="req-path-1"]').first();
  const always = card.getByRole('button', { name: /^Always allow/ });
  await always.scrollIntoViewIfNeeded();
  await expect(always).toBeInViewport();
  const box = await card.boundingBox();
  expect(box!.x + box!.width).toBeLessThanOrEqual(390);
  await page.close();
});
