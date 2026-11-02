import { expect, test } from '@playwright/test';
import { header, login, send, sessionRow, step, timeline } from './helpers';

// The agent dies mid-turn (the test script kills pi while the model holds its reply), so the
// session is interrupted. Start fresh with the previous conversation: a divider marks the new
// agent session, and the next run works (the test script checks pi was pointed at the history
// export and could read it in the sandbox).
test('start fresh with the previous conversation after the agent died', async ({ page }) => {
  await login(page, 'Laptop');
  await sessionRow(page, 'hello').click();
  const tl = timeline(page);
  await send(page, 'Think it over');
  await expect(tl.locator('.msg.r-agent').last()).toHaveText('Thinking it over');
  await step('crash');

  await expect(header(page).locator('.status')).toHaveText('Interrupted');
  const banner = page.locator('.banner', { hasText: 'Agent stopped mid-turn' });
  await banner.getByRole('button', { name: 'Start fresh…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Start fresh' });
  await dialog.getByLabel('Give the agent the previous conversation').check();
  await dialog.getByRole('button', { name: 'Start fresh' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(banner).toHaveCount(0);
  await expect(tl.getByRole('separator', { name: 'Started fresh' })).toBeVisible();
  await step('fresh');

  await send(page, 'What did we talk about?');
  await expect(tl.locator('.msg.r-agent').last()).toHaveText('I read the earlier conversation.');
  // One divider, between the old agent session's last words and the new one's first message.
  await expect(tl.getByRole('separator', { name: 'Started fresh' })).toHaveCount(1);
  const order = await tl.locator('.row').evaluateAll((rows) =>
    rows.flatMap((r) => (r.querySelector('[role="separator"]') ? ['divider', r.textContent ?? ''] : [r.textContent ?? ''])),
  );
  const at = (pred: (row: string) => boolean) => order.findIndex(pred);
  const divider = at((r) => r === 'divider');
  expect(at((r) => r.includes('Thinking it over'))).toBeLessThan(divider);
  expect(divider).toBeLessThan(at((r) => r.includes('What did we talk about?')));
  await expect(header(page).locator('.status')).toHaveText('Complete');
});
