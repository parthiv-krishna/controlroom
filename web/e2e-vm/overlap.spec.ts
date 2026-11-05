import { expect, test } from '@playwright/test';
import { header, login, newSession, openSession } from './helpers';

// A second session on devbox:~/work/api (where "two" works): allowed, with a warning in the
// new-session dialog and, once the server sees the overlap, in both sessions' headers.
// Archiving the second session clears it.
test('a second session on the same directory warns about the overlap', async ({ page }) => {
  await login(page, 'Laptop');
  await newSession(page, { workspaces: [{ machine: 'devbox', path: '~/work/api' }], name: 'notes', level: 'Strict', overlap: 'two' });
  const overlap = header(page).locator('[data-overlap]');
  await expect(overlap).toContainText('shared with two');
  await expect(overlap).toHaveAttribute('title', /^devbox:~\/work\/api/);

  await openSession(page, 'two');
  await expect(overlap).toContainText('shared with notes');
  // The warning links the other session.
  await overlap.getByRole('button', { name: 'notes' }).click();
  await expect(header(page).locator('.et.heading')).toHaveText('notes');

  await header(page).getByRole('button', { name: 'Archive' }).click();
  // Its agent is live (idle): archiving stops it.
  await page.getByRole('alertdialog', { name: 'Stop and archive?' }).getByRole('button', { name: 'Stop and archive' }).click();
  await expect(header(page).locator('.status')).toHaveText('Archived');
  await openSession(page, 'two');
  await expect(overlap).toHaveCount(0);
});
