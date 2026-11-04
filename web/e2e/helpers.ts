import { expect, type Page } from '@playwright/test';
import type { RecordedCall } from '../src/lib/api/fixture';

/** Fixture ids (web/fixtures/controlroom.json). */
export const M = { devbox: '01JAM000000000000000000001', buildbox: '01JAM000000000000000000002' };
export const S = {
  auth: '01JAS000000000000000000001',
  flaky: '01JAS000000000000000000002',
  jobs: '01JAS000000000000000000003',
  release: '01JAS000000000000000000004',
  blog: '01JAS000000000000000000006',
  coord: '01JAS000000000000000000011',
  svelte: '01JAS000000000000000000009',
  sweep8k: '01JAS000000000000000000013',
  dedupe: '01JAS000000000000000000014',
  deploy: '01JAS000000000000000000015',
};

/** Opens the fixture build with the scripted feed paused (deterministic), optionally at a session. */
export async function start(page: Page, session?: string, query = 'paused'): Promise<void> {
  await page.goto(`/?${query}${session ? `#/s/${session}` : ''}`);
  await expect(page.getByRole('listbox', { name: 'Session list' })).toBeVisible();
}

/** The calls the app made, as recorded by the fixture transport. */
export async function calls(page: Page, method?: string): Promise<RecordedCall[]> {
  const all = await page.evaluate(() => window.__fixture?.calls ?? []);
  return method ? all.filter((c) => c.method === method) : all;
}

/** Plays a session's next `n` recorded timeline steps (web/fixtures/timelines.json). */
export async function advance(page: Page, id: string, n = 1): Promise<number> {
  return page.evaluate(({ id, n }) => window.__fixture?.advance(id, n) ?? 0, { id, n });
}
