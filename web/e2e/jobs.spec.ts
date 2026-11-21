import { expect, test, type Page } from '@playwright/test';
import type { Job, SessionDetail } from '../src/lib/protocol';
import { calls, S, start } from './helpers';

// Background jobs: the strip shows running jobs only, in one row (on a phone too, whatever the
// count); finished ones are in the timeline and in the Jobs sheet (the session's ⋯ menu). A job
// wake, in the production shape too, is the job's row, never a user message.

const BUILDBOX = '01JAM000000000000000000002';

/** `n` running jobs, the first followed. */
function running(n: number): Job[] {
  return Array.from({ length: n }, (_, i) => ({
    id: `01JAJ0000000000000000001${String(i).padStart(2, '0')}`,
    name: `suite ${i + 1}`,
    command: `pnpm e2e --shard ${i + 1}/${n}`,
    cwd: '/home/alice/src/controlroom',
    status: 'running',
    exit: null,
    signal: null,
    started_at: Date.now() - (i + 1) * 60_000,
    ended_at: null,
    timeout_s: null,
    log_path: `/home/alice/src/controlroom/.controlroom/jobs/${i}.log`,
    lines: 10 * i,
    follow: i === 0 ? { buffer_seconds: 15, pattern: 'FAIL', next_wake_at: null } : null,
    machine_id: BUILDBOX,
    workspace_id: null,
  }));
}

/** Replaces the jobs session's jobs with `jobs`. */
async function setJobs(page: Page, jobs: Job[]): Promise<void> {
  await page.evaluate(
    ({ id, jobs }) => {
      const f = window.__fixture!;
      const detail = { ...(f as unknown as { fixture: { details: Record<string, SessionDetail> } }).fixture.details[id], jobs };
      f.setDetail(id, detail);
    },
    { id: S.jobs, jobs },
  );
}

test('the strip shows running jobs only; Jobs (N) in ⋯ lists them all', async ({ page }) => {
  await start(page, S.jobs);
  const strip = page.getByLabel('Running jobs');
  await expect(strip.getByRole('button')).toHaveCount(1);
  await expect(strip.getByRole('button', { name: /^Job tests, running/ })).toBeVisible();
  if (process.env.CR_SCREENS) await page.screenshot({ path: `${process.env.CR_SCREENS}/jobs-desktop.png` });

  await page.getByRole('button', { name: 'Session actions' }).click();
  await page.getByRole('menuitem', { name: 'Background jobs (3)' }).click();
  const sheet = page.getByRole('dialog', { name: 'Jobs' });
  const list = sheet.getByRole('list', { name: 'Jobs' });
  await expect(list.getByRole('button', { name: /^Job / })).toHaveText([/tests/, /typecheck/, /lint/]);
  await list.getByRole('button', { name: /^Job lint/ }).click();
  await expect(sheet.getByRole('button', { name: /^Copy log path/ })).toBeVisible();
  await expect(sheet.getByRole('button', { name: /^Kill/ })).toHaveCount(0);
  await sheet.getByRole('button', { name: 'Close jobs' }).click();

  // A chip opens its job: log tail, unfollow, kill.
  await strip.getByRole('button', { name: /^Job tests/ }).click();
  await expect(sheet.getByLabel('Log tail')).toContainText('FAIL e2e/queue.spec.ts');
  await sheet.getByRole('button', { name: 'Unfollow' }).click();
  await sheet.getByRole('button', { name: 'Kill job tests' }).click();
  const methods = (await calls(page)).map((c) => c.method);
  expect(methods).toEqual(expect.arrayContaining(['job.logs', 'job.unfollow', 'job.kill']));
});

test('nothing runs: no strip', async ({ page }) => {
  await start(page, S.jobs);
  await expect(page.getByLabel('Running jobs')).toBeVisible();
  await setJobs(page, []);
  await expect(page.getByLabel('Running jobs')).toHaveCount(0);
});

test('a wake in the production shape is its job row, not a user message', async ({ page }) => {
  await start(page, S.jobs);
  const main = page.getByRole('main');
  const row = main.getByRole('button', { name: /Job Final PaaS validation failed \(exit 1\)/ });
  await expect(row).toHaveCount(1);
  await expect(main.getByText('Background job update')).toHaveCount(0);
  // The exact text the agent got is one click away.
  const item = main.locator('[data-kind="job"]').filter({ hasText: 'Final PaaS validation' });
  await item.getByRole('button', { name: 'told' }).click();
  await expect(item).toContainText('Followed lines not reported yet: lines 81, 115, 186, 188-189, 216, 218.');
  if (process.env.CR_SCREENS) await item.screenshot({ path: `${process.env.CR_SCREENS}/wake-pill-told.png` });
});

test.describe('phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  for (const n of [0, 1, 5, 20]) {
    test(`${n} running jobs: one row at most; the composer and the last item stay in view`, async ({ page }) => {
      await start(page);
      await page.goto(`/?paused#/s/${S.jobs}`);
      await expect(page.getByLabel('Message')).toBeVisible();
      await setJobs(page, running(n));
      const strip = page.getByLabel('Running jobs');
      if (n === 0) await expect(strip).toHaveCount(0);
      else {
        await expect(strip.getByRole('button')).toHaveCount(1);
        await expect(strip).toContainText(n === 1 ? 'suite 1' : `${n} running`);
        const box = (await strip.boundingBox())!;
        expect(box.height).toBeLessThanOrEqual(28);
      }
      await expect(page.getByLabel('Message')).toBeInViewport();
      await expect(page.getByRole('main').locator('[data-item]').last()).toBeInViewport();
      if (process.env.CR_SCREENS) await page.screenshot({ path: `${process.env.CR_SCREENS}/jobs-phone-${n}.png` });
      if (n > 1) {
        await strip.getByRole('button').click();
        const sheet = page.getByRole('dialog', { name: 'Jobs' });
        await expect(sheet.getByRole('list', { name: 'Jobs' }).getByRole('button', { name: /^Job / })).toHaveCount(n);
        await expect(sheet.getByRole('button', { name: 'Close jobs' })).toBeInViewport();
        if (process.env.CR_SCREENS) await page.screenshot({ path: `${process.env.CR_SCREENS}/jobs-phone-${n}-sheet.png` });
      }
    });
  }
});
