import { expect, test } from '@playwright/test';
import { lastReply, login, openSession, requestCards, send, settled, toolCalls } from './helpers';

// The agent's command reads from the terminal: devbox's host daemon notices it waits for input,
// the session raises a terminal-input request, and the user answers by typing in that shell's
// terminal tab (xterm over the controller's terminal stream to `controlroomd pty attach`).
test('a command waiting for input is answered in the terminal tab', async ({ page }) => {
  await login(page, 'Laptop');
  await openSession(page, 'two');
  await send(page, 'Ask me for a name');

  const card = requestCards(page, 'terminal_input').last();
  await expect(card).toHaveClass(/\bpending\b/);
  await expect(card.locator('[data-shell]')).toContainText('devbox');
  await card.getByRole('button', { name: 'Open terminal “devbox”' }).click();

  const dock = page.getByRole('region', { name: 'Terminals' });
  await expect(dock.getByRole('tab', { name: /^devbox/ })).toHaveAttribute('aria-selected', 'true');
  await expect(dock.getByRole('status')).toContainText('Waiting for input');
  await expect(dock.locator('.xterm-rows')).toContainText('Name?');
  await dock.locator('.xterm').click();
  await page.keyboard.type('alice');
  await page.keyboard.press('Enter');

  await expect(requestCards(page, 'terminal_input.pending')).toHaveCount(0);
  await expect(toolCalls(page, '# ask')).toHaveClass(/s-completed/);
  await expect(lastReply(page)).toHaveText('Greeted.');
  await expect(dock.locator('.xterm-rows')).toContainText('hello alice');
  await settled(page);
});
