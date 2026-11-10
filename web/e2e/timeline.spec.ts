import { expect, test, type Page } from '@playwright/test';
import { S, advance, calls, start } from './helpers';

// Sessions with recorded timelines (web/fixtures/timelines.json, see scripts/timeline-fixtures.mjs).
const sweep = S.sweep8k; // backlog plain-text; live thinking, nested-tool
const release = S.release; // live permission-dialog after a backlog
const svelte = S.svelte; // nested-tool, parallel-tools, … (static)
const blog = S.blog; // a long timeline (310 items)

const timeline = (page: Page) => page.getByRole('main').getByLabel('Timeline');
const item = (page: Page, id: string) => timeline(page).locator(`[data-item="${id}"]`);

test('streaming text patches the message in place (identity, focus and caret survive)', async ({ page }) => {
  await start(page, sweep);
  await expect(item(page, 'user-2')).toBeVisible();
  await advance(page, sweep, 10); // thinking streams, then the reply starts
  const reply = item(page, 'c1.msg-11.1');
  await expect(reply).toContainText('The answ');
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    w.__reply = document.querySelector('[data-item="c1.msg-11.1"] .md');
    w.__para = document.querySelector('[data-item="c1.msg-11.1"] .md p');
  });
  const composer = page.getByLabel('Message');
  await composer.fill('draft text');
  await composer.evaluate((el: HTMLTextAreaElement) => el.setSelectionRange(3, 3));
  const before = await reply.textContent();
  await advance(page, sweep, 3); // the rest of the text, then end
  await expect.poll(() => reply.textContent()).not.toBe(before);
  const same = await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    return w.__reply === document.querySelector('[data-item="c1.msg-11.1"] .md') && w.__para === document.querySelector('[data-item="c1.msg-11.1"] .md p');
  });
  expect(same).toBe(true);
  await expect(composer).toBeFocused();
  expect(await composer.evaluate((el: HTMLTextAreaElement) => el.selectionStart)).toBe(3);
  // The thinking item is collapsible and its text streamed in too.
  await item(page, 'c1.msg-11.0').getByRole('button', { name: /Thinking/ }).click();
  await expect(item(page, 'c1.msg-11.0')).toContainText('Let me think about it.');
  // Stuck to the bottom while streaming: the newest item is in view.
  await advance(page, sweep, 3);
  await expect(item(page, 'c1.run-5')).toBeInViewport();
});

test('nested tool calls render inside their parent, live and from the backlog', async ({ page }) => {
  await start(page, svelte);
  // The nested call is near the top of a long timeline: scroll it into the rendered window.
  await timeline(page).evaluate((el) => (el.scrollTop = 0));
  const parent = item(page, 'msg-11.0');
  await expect(parent).toContainText('fixture_nested');
  await expect(parent.locator('[data-item="tool-18"]')).toContainText('notes.txt');
  await expect(parent.getByText('1 sub-call')).toBeVisible();

  await start(page, sweep);
  await advance(page, sweep, 36);
  // A finished call shows no status (it's in its title and its expanded detail).
  const nested = item(page, 'c2.msg-11.0').locator('[data-item="c2.tool-18"]');
  await expect(nested.locator('.head')).toHaveAttribute('title', /^completed/);
  await expect(nested.locator('.st')).toHaveCount(0);
});

test('a dialog streamed into the timeline is pinned above the composer; the timeline keeps a stub', async ({ page }) => {
  await start(page, release);
  await advance(page, release, 9); // up to the pending permission dialog
  const card = item(page, 'c3.dialog-18');
  const tray = page.getByRole('main').getByRole('region', { name: 'Requests' });
  const pinned = tray.locator('[data-request="c3.dialog-18"]');
  await expect(pinned).toContainText('Allow: echo fixture-guard ok?');
  // The timeline shows a stub that jumps to the pinned card.
  await expect(card.locator('[data-request]')).toHaveCount(0);
  await card.locator('[data-stub="c3.dialog-18"]').click();
  await expect(pinned.getByRole('button', { name: 'Allow' })).toBeFocused();
  await pinned.getByRole('button', { name: 'Allow' }).click();
  await expect.poll(async () => (await calls(page, 'request.respond')).map((c) => c.params)).toContainEqual(
    expect.objectContaining({ id: S.release, request_id: 'c3.dialog-18', response: { kind: 'choice', option_id: '0' } }),
  );
  // The recording's answer arrives: the tray empties and the timeline shows the outcome.
  await advance(page, release, 1);
  await expect(card).toContainText('→ Allow');
  await expect(tray).toHaveCount(0);
});

test('raw view: an item opens its records, and a record links back to its item', async ({ page }) => {
  await start(page, svelte);
  await timeline(page).evaluate((el) => (el.scrollTop = 0));
  const tool = item(page, 'tool-18');
  await tool.hover();
  await tool.getByRole('button', { name: /^Raw records #\d+/ }).last().click();
  const raw = page.getByLabel('Raw transcript');
  await expect(raw).toBeVisible();
  const subs = await calls(page, 'session.subscribe');
  expect(subs.at(-1)?.params).toMatchObject({ id: S.svelte, raw_since: expect.any(Number) });
  const focused = raw.locator('.line.focus');
  await expect(focused.first()).toBeVisible();
  await focused.first().getByRole('button', { name: /^#\d+/ }).click();
  await expect(raw.locator('pre').first()).toContainText('"type"');
  await focused.last().getByRole('button', { name: 'item ↗' }).click();
  await expect(raw).toHaveCount(0);
  await expect(item(page, 'tool-18')).toBeInViewport();
  // Closing the raw view resubscribes without raw frames.
  await expect.poll(async () => (await calls(page, 'session.subscribe')).at(-1)?.params).toMatchObject({ raw_since: null });
});

test('a long timeline is windowed and opens at the bottom', async ({ page }) => {
  await start(page, blog);
  const rows = timeline(page).locator('.row[data-row]');
  await expect(rows.first()).toBeVisible();
  const total = 220; // about that many rows: 310 items, less the turns' usage and nested calls
  expect(await rows.count()).toBeLessThan(total / 3);
  // The latest shown item (the last reply; its turn's usage is hidden) is in view; the first one
  // isn't in the DOM.
  await expect(item(page, 'c39.msg-25.0')).toBeInViewport();
  await expect(item(page, 'user-2')).toHaveCount(0);
  // Scrolling to the top renders the first rows instead.
  await timeline(page).evaluate((el) => (el.scrollTop = 0));
  await expect(item(page, 'user-2')).toBeInViewport();
  expect(await rows.count()).toBeLessThan(total / 3);
});
