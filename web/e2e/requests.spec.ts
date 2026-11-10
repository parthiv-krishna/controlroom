import { expect, test, type Page } from '@playwright/test';
import { S, calls, start } from './helpers';

// Open requests are pinned in a tray right above the composer (the timeline keeps a stub).
const tray = (page: Page) => page.getByRole('main').getByRole('region', { name: 'Requests' });

test('requests: pinned above the composer, oldest first, with a count; a subagent\'s too', async ({ page }) => {
  await start(page, S.coord);
  const pinned = tray(page).locator('[data-pinned]');
  await expect(tray(page)).toContainText('2 requests');
  await expect(pinned).toHaveCount(2);
  // The tray sits directly above the composer.
  const trayBox = await tray(page).boundingBox();
  const composer = await page.getByLabel('Message').boundingBox();
  expect(trayBox!.y + trayBox!.height).toBeLessThanOrEqual(composer!.y);
  // A subagent's request is answered on the subagent, from the parent's tray.
  const sub = tray(page).locator('[data-request="req-3"]');
  await expect(sub).toContainText('from subagent');
  await sub.getByRole('button', { name: 'Allow once' }).click();
  await expect.poll(async () => (await calls(page, 'request.respond')).at(-1)?.params).toMatchObject({ id: '01JAS000000000000000000012', request_id: 'req-3' });

  // One request: no count.
  await start(page, S.auth);
  await expect(tray(page).locator('[data-pinned]')).toHaveCount(1);
  await expect(tray(page)).not.toContainText('requests');
});

test('requests: on a phone the tray is capped and the composer stays on screen', async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 390, height: 700 }, hasTouch: true, isMobile: true });
  await page.goto(`/?paused#/s/${S.coord}`);
  await expect(tray(page).locator('[data-pinned]')).toHaveCount(2);
  const composer = page.getByLabel('Message');
  await expect(composer).toBeInViewport();
  const trayBox = await tray(page).boundingBox();
  const pane = await page.getByRole('main').boundingBox();
  expect(trayBox!.height).toBeLessThanOrEqual(pane!.height * 0.45);
  await page.close();
});
