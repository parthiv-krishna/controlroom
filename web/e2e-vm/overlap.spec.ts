import { expect, test } from '@playwright/test';
import { header, sessionAction, login, newSession, openSession } from './helpers';

// A second session on devbox:~/work/api (where "two" works): allowed, with a warning in the
// new-session dialog and, once the server sees the overlap, in both sessions' headers ("notes"
// has one workspace: its pill's ⚠; "two" has two: a ⚠ on its workspaces chip and the warning in
// its list). Archiving the second session clears it.
test('a second session on the same directory warns about the overlap', async ({ page }) => {
  await login(page, 'Laptop');
  await newSession(page, { workspaces: [{ machine: 'devbox', path: '~/work/api' }], name: 'notes', level: 'Strict', overlap: 'two' });
  await expect(header(page).locator('[data-overlap]')).toHaveAttribute('title', /^devbox:~\/work\/api is shared with two/);

  await openSession(page, 'two');
  const chip = header(page).getByRole('button', { name: /^2 workspaces/ });
  await expect(chip).toHaveAccessibleName('2 workspaces, 1 shared with another session');
  await expect(chip).toHaveAttribute('title', /devbox:~\/work\/api is shared with notes/);
  // The warning links the other session.
  await chip.click();
  const api = page.getByRole('dialog', { name: 'Session workspaces' }).locator('[data-workspace]').filter({ hasText: 'devbox:~/work/api' });
  await api.locator('[data-overlap]').getByRole('button', { name: 'notes' }).click();
  await expect(header(page).locator('.et.heading')).toHaveText('notes');

  await sessionAction(page, 'Archive');
  // Its agent is live (idle): archiving stops it.
  await page.getByRole('alertdialog', { name: 'Stop and archive?' }).getByRole('button', { name: 'Stop and archive' }).click();
  await expect(header(page).locator('.status')).toHaveText('Archived');
  await openSession(page, 'two');
  await expect(chip).toHaveAccessibleName('2 workspaces');
});
