import { devices, expect, test } from '@playwright/test';
import { header, login, newSession, sessionRow, step, timeline } from './helpers';

// A sandboxed pi session from the new-session dialog: the first message runs at once and the
// mock model's reply streams into the timeline (held at a model gate until the test script
// releases it). Nobody is looking when the run ends, so the session settles idle and unread;
// opening it on the phone reads it.
test('a sandboxed pi session streams its reply and settles unread', async ({ page, browser }) => {
  await login(page, 'Laptop');
  await newSession(page, { cwd: '~/work', name: 'hello', level: 'Strict', prompt: 'Say hello' });

  const head = header(page);
  await expect(head.locator('.sbx')).toHaveText('strict');
  const tl = timeline(page);
  await expect(tl.locator('.msg.r-user')).toHaveText('Say hello');
  // The first part of the reply has streamed in; the rest waits at the model gate.
  const reply = tl.locator('.msg.r-agent');
  await expect(reply).toHaveText('Hello from');
  await expect(reply).toHaveClass(/streaming/);
  await expect(head.locator('.status')).toHaveText('Working');
  // Opening a session reads it, so the laptop leaves before the run ends.
  await page.close();

  const phone = await browser.newContext({ ...devices['Pixel 7'] });
  const mobile = await phone.newPage();
  await login(mobile, 'Phone');
  const row = sessionRow(mobile, 'hello');
  await expect(row).toBeVisible();
  await step('streaming');

  await expect(row).toHaveClass(/\bunread\b/);
  await expect(row.locator('.prev')).toHaveText('Hello from the mock model.');
  await row.click();
  await expect(timeline(mobile).locator('.msg.r-agent')).toHaveText('Hello from the mock model.');
  await expect(mobile.locator('.back .muted')).toHaveText('Complete');
  await mobile.getByRole('button', { name: '‹ Sessions' }).click();
  await expect(row).not.toHaveClass(/\bunread\b/);
  await phone.close();
});
