import { expect, test, type Page } from '@playwright/test';
import { S, start } from './helpers';

// Fixture sessions (web/fixtures/controlroom.json): "benchmark coordinator" has shells on two
// workspaces (devbox idle, buildbox running an agent command); "e2e flake hunt" has a shell that
// waits for input, with its terminal_input request.
const coord = S.coord;
const waiting = S.jobs;

/** What the fixture's terminal streams received. */
async function terminalLog(page: Page) {
  return page.evaluate(() => window.__fixture?.terminals.log ?? []);
}

const inTerminal = (page: Page) => page.evaluate(() => !!document.activeElement?.closest('.xt'));

test('dock: tabs per shell grouped by workspace; xterm loads on open; typing and resize reach the stream', async ({ page }) => {
  const scripts: string[] = [];
  page.on('request', (r) => scripts.push(r.url()));
  await start(page, coord);
  const dock = page.getByRole('region', { name: 'Terminals' });
  const tabs = dock.getByRole('tablist', { name: 'Shells' });
  await expect(tabs).toContainText('devbox:~/src/bench');
  await expect(tabs).toContainText('buildbox:~/src/bench');
  await expect(tabs.getByRole('tab', { name: /buildbox/ })).toContainText('agent');
  expect(scripts.some((u) => /xterm/.test(u))).toBe(false);

  await tabs.getByRole('tab', { name: /^devbox$/ }).click();
  const rows = dock.locator('.xterm-rows');
  await expect(rows).toContainText('cargo test -q');
  await expect(dock.locator('.xt-agent')).toHaveCount(1);
  expect(scripts.some((u) => /xterm/.test(u))).toBe(true);

  await dock.locator('.xterm').click();
  await page.keyboard.type('echo hi');
  await page.keyboard.press('Enter');
  await expect(rows).toContainText('echo: ran in the fixture');
  const [log] = await terminalLog(page);
  expect(log.target).toEqual({ session: coord, shell: 'devbox' });
  expect(log.inputs.join('')).toBe('echo hi\r');
  expect(log.resizes.length).toBeGreaterThan(0);

  // Another tab is another stream; closing the dock ends them.
  await tabs.getByRole('tab', { name: /buildbox/ }).click();
  await expect.poll(async () => (await terminalLog(page)).map((l) => [l.target.shell, l.closed])).toEqual([
    ['devbox', true],
    ['buildbox', false],
  ]);
  await dock.getByRole('button', { name: 'Close terminals' }).click();
  await expect(rows).toHaveCount(0);
  await expect.poll(async () => (await terminalLog(page)).every((l) => l.closed)).toBe(true);
});

test('keys: Ctrl-h/j/k/l navigate unless a pass-through program is in the foreground; Ctrl-\\ sends the next key', async ({ page }) => {
  await start(page, coord);
  const dock = page.getByRole('region', { name: 'Terminals' });
  await dock.getByRole('tab', { name: /^devbox$/ }).click();
  await expect(dock.locator('.xterm-rows')).toContainText('cargo test -q');
  await dock.locator('.xterm').click();
  expect(await inTerminal(page)).toBe(true);

  // At the shell prompt the navigator moves focus out of the terminal.
  await page.keyboard.press('Control+k');
  await expect.poll(() => inTerminal(page)).toBe(false);

  await dock.locator('.xterm').click();
  await page.keyboard.type('vim');
  await page.keyboard.press('Enter');
  await expect(dock.locator('.meta.pass')).toContainText('vim in the foreground · Ctrl-h/j/k/l go to it');
  await page.keyboard.press('Control+k');
  expect(await inTerminal(page)).toBe(true);
  await page.keyboard.type(':q');
  await page.keyboard.press('Enter');
  await expect(dock.locator('.meta')).toHaveCount(0);

  // The escape hatch: Ctrl-\ then Ctrl-l clears the shell's screen instead of moving focus.
  await page.keyboard.press('Control+\\');
  await expect(dock.locator('.xt-prefix')).toBeVisible();
  await page.keyboard.press('Control+l');
  expect(await inTerminal(page)).toBe(true);
  const [log] = await terminalLog(page);
  expect(log.inputs).toContain('\x0b');
  expect(log.inputs).toContain('\x0c');
});

test('a terminal_input request opens and focuses the waiting shell', async ({ page }) => {
  await start(page, waiting);
  const card = page.locator('[data-request="req-term-1"]');
  await card.getByRole('button', { name: 'Open terminal “buildbox”' }).click();
  const dock = page.getByRole('region', { name: 'Terminals' });
  await expect(dock.getByRole('tab', { name: /buildbox/ })).toHaveAttribute('aria-selected', 'true');
  await expect(dock.getByRole('status').filter({ hasText: 'Waiting for input' })).toBeVisible();
  await expect.poll(() => inTerminal(page)).toBe(true);
  await page.keyboard.type('sudo true');
  await page.keyboard.press('Enter');
  await page.keyboard.type('s3cret');
  await page.keyboard.press('Enter');
  await expect(dock.locator('.xterm-rows')).toContainText('authenticated');
  await expect(dock.locator('.xterm-rows')).not.toContainText('s3cret');
});

test('phone: the terminal opens full screen with an extra-keys row', async ({ browser }) => {
  const ctx = await browser.newContext({ viewport: { width: 393, height: 851 }, isMobile: true, hasTouch: true });
  const page = await ctx.newPage();
  await page.goto(`/?paused#/s/${coord}`);
  const dock = page.getByRole('region', { name: 'Terminals' });
  await dock.getByRole('tab', { name: /^devbox$/ }).click();
  await expect(page.locator('.dock.phone.open')).toBeVisible();
  await expect(dock.locator('.xterm-rows')).toContainText('cargo test -q');
  const keys = page.getByRole('toolbar', { name: 'Extra keys' });
  for (const k of ['Esc', 'Tab', 'Ctrl', '↑']) await expect(keys.getByRole('button', { name: k, exact: true })).toBeVisible();
  await keys.getByRole('button', { name: '↑', exact: true }).click();
  await keys.getByRole('button', { name: 'Ctrl', exact: true }).click();
  await dock.locator('.xterm').click();
  await page.keyboard.type('c');
  await expect.poll(async () => (await terminalLog(page))[0]?.inputs).toEqual(['\x1b[A', '\x03']);
  await dock.getByRole('button', { name: 'Back' }).click();
  await expect(page.locator('.dock.phone.open')).toHaveCount(0);
  await ctx.close();
});
