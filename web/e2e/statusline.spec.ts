import { expect, test, type Page } from '@playwright/test';
import { S, start } from './helpers';

// The session's usage in the header, in pi's footer format, instead of a usage line per turn.
// The blog session's summary carries pi's footer example (web/fixtures/controlroom.json).
const PI_EXAMPLE = '↑430k ↓378k R303M W54M CH99.3% $337.700 16.0%/1.0M';

const header = (page: Page) => page.locator('[data-pane="header"]');
const statusline = (page: Page) => header(page).getByRole('button', { name: 'Session usage' });
const shownText = async (page: Page) => (await statusline(page).locator('[data-part]').allTextContents()).join(' ');

test('the header shows the session usage like pi; the timeline has no per-turn usage line', async ({ page }) => {
  await start(page, S.blog);
  await expect(statusline(page)).toBeVisible();
  expect(await shownText(page)).toBe(PI_EXAMPLE);
  const timeline = page.getByRole('main').getByLabel('Timeline');
  await expect(timeline.locator('[data-item]').first()).toBeVisible();
  await expect(timeline.locator('[data-kind="usage"]')).toHaveCount(0);
  await expect(timeline.getByText(/^turn ·/)).toHaveCount(0);
});

test('hovering shows every part with its label; a click pins it, Escape closes it', async ({ page }) => {
  await start(page, S.blog);
  const pop = page.getByRole('dialog', { name: 'Session usage' });
  await statusline(page).hover();
  await expect(pop).toBeVisible();
  await expect(pop.locator('[data-part="cache_write"]')).toContainText('W54M');
  await expect(pop.locator('[data-part="cache_write"]')).toContainText('cache write');
  await expect(pop.locator('[data-part="cache_write"]')).toContainText('54,000,000');
  await page.mouse.move(0, 0);
  await expect(pop).toBeHidden();
  await statusline(page).click();
  await page.mouse.move(0, 0);
  await expect(pop).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(pop).toBeHidden();
});

test('context fill above 70 % warns, above 90 % errs', async ({ page }) => {
  await start(page, S.auth);
  await expect(statusline(page).locator('[data-part="context"]')).toHaveClass(/warn/);
  await expect(statusline(page).locator('[data-part="context"]')).toHaveText('75.0%/200k');
  await start(page, '01JAS000000000000000000012');
  await expect(statusline(page).locator('[data-part="context"]')).toHaveClass(/full/);
});

test('phone: the statusline fits the header, dropping W, then R, then CH; a tap shows them all', async ({ browser }) => {
  for (const [width, text] of [
    [390, '↑430k ↓378k R303M CH99.3% $337.700 16.0%/1.0M'],
    [320, '↑430k ↓378k $337.700 16.0%/1.0M'],
  ] as const) {
    const page = await browser.newPage({ viewport: { width, height: 800 }, hasTouch: true, isMobile: true });
    await page.goto(`/?paused#/s/${S.blog}`);
    await expect(statusline(page)).toBeVisible();
    await expect.poll(() => shownText(page)).toBe(text);
    const line = await statusline(page).boundingBox();
    const head = await header(page).boundingBox();
    expect(line!.x + line!.width).toBeLessThanOrEqual(head!.x + head!.width + 0.5);
    await statusline(page).tap();
    const pop = page.getByRole('dialog', { name: 'Session usage' });
    await expect(pop.locator('tr')).toHaveCount(7);
    await expect(pop).toContainText('cache write');
    await page.close();
  }
});
