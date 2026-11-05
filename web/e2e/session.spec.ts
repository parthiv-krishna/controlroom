import { expect, test } from '@playwright/test';
import { S, calls, start } from './helpers';

test('request card: Allow once records request.respond on the session', async ({ page }) => {
  await start(page, S.auth);
  const card = page.getByRole('main').locator('[data-request="req-2"]');
  await expect(card.getByText('Edit src/auth/refresh.rs')).toBeVisible();
  await card.getByRole('button', { name: 'Allow once' }).click();
  await expect.poll(async () => (await calls(page, 'request.respond')).map((c) => c.params)).toContainEqual(
    expect.objectContaining({ id: S.auth, request_id: 'req-2', response: { kind: 'choice', option_id: 'allow_once' } }),
  );
});

test('request card: Reject & steer denies and sends the text', async ({ page }) => {
  await start(page, S.auth);
  const card = page.getByRole('main').locator('[data-request="req-2"]');
  await card.getByRole('button', { name: 'Reject & steer…' }).click();
  await card.getByLabel('Steer').fill('Use a mutex instead');
  await card.getByLabel('Steer').press('Enter');
  const [call] = await calls(page, 'request.respond');
  expect(call.params).toMatchObject({ response: { kind: 'choice', option_id: 'reject' }, then: [{ kind: 'text', text: 'Use a mutex instead' }] });
});

test('a subagent request rolled up to the parent is answered on the child', async ({ page }) => {
  await start(page, S.coord);
  const card = page.getByRole('main').locator('[data-request="req-3"]');
  await expect(card.getByText('from subagent')).toBeVisible();
  await card.getByRole('button', { name: 'Allow once' }).click();
  const [call] = await calls(page, 'request.respond');
  expect(call.params).toMatchObject({ id: '01JAS000000000000000000012', request_id: 'req-3' });
});

test('composer: Enter queues while busy, Esc stops; queue items show sending and send now', async ({ page }) => {
  await start(page, S.flaky);
  const queue = page.getByLabel('Queue');
  await expect(queue.getByText('sending')).toBeVisible();
  await queue.getByRole('button', { name: '⚡ Send now' }).first().click();
  const composer = page.getByLabel('Message');
  await composer.fill('Also check the retry logic');
  await expect(page.getByRole('button', { name: '↵ Queue' })).toBeVisible();
  await composer.press('Enter');
  await composer.press('Escape');
  const methods = (await calls(page)).map((c) => c.method);
  expect(methods).toEqual(expect.arrayContaining(['queue.send_now', 'session.prompt', 'session.cancel']));
  const [prompt] = await calls(page, 'session.prompt');
  expect(prompt.params).toMatchObject({ content: [{ kind: 'text', text: 'Also check the retry logic' }], if_busy: 'queue', origin: { kind: 'user' } });
});

test('scripted feed: the in-flight queue item is echoed and leaves the queue', async ({ page }) => {
  await start(page, S.flaky);
  const queue = page.getByLabel('Queue');
  await expect(queue.getByText('Re-run only the failing spec')).toBeVisible();
  await page.evaluate(() => {
    window.__fixture?.step();
    window.__fixture?.step();
  });
  await expect(queue.getByText('Re-run only the failing spec')).toHaveCount(0);
  await expect(page.getByRole('option', { name: /flaky e2e tests/ })).toContainText('Re-running the failing spec');
});

test('todo panel and auto-continue; jobs strip', async ({ page }) => {
  await start(page, S.jobs);
  const todo = page.getByLabel('Todo');
  await expect(todo.getByText('Auto-continue')).toContainText('on');
  await expect(todo.getByText('◷ waiting on job tests')).toBeVisible();
  await todo.getByLabel('Add todo').fill('Write the regression test');
  await todo.getByLabel('Add todo').press('Enter');
  await todo.getByRole('button', { name: 'Turn off' }).click();
  await page.getByRole('button', { name: /tests/ }).first().click();
  await expect(page.getByLabel('Log tail')).toContainText('FAIL e2e/queue.spec.ts');
  await expect(page.getByText(/next wake in \d+s/)).toBeVisible();
  await page.getByRole('button', { name: 'Unfollow' }).click();
  await page.getByRole('button', { name: 'Kill' }).click();
  const methods = (await calls(page)).map((c) => c.method);
  expect(methods).toEqual(expect.arrayContaining(['todo.add', 'session.update', 'job.logs', 'job.unfollow', 'job.kill']));
});

test('notifications: blocking and non-blocking tiers; Dismiss answers declined', async ({ page }) => {
  await start(page);
  await page.locator('body').press(']');
  const panel = page.getByRole('complementary', { name: 'Notifications' });
  await expect(panel.getByText('Needs you')).toBeVisible();
  await expect(panel.getByText('Questions', { exact: true })).toBeVisible();
  await expect(panel.getByText('Failed or stopped')).toBeVisible();
  await expect(panel.getByText('Finished')).toBeVisible();
  await expect(panel.getByText(/gpubox.*1 session with a workspace there/)).toBeVisible();
  await panel.locator('[data-request="req-12"]').getByRole('button', { name: 'Dismiss' }).click();
  const [call] = await calls(page, 'request.respond');
  expect(call.params).toMatchObject({ id: S.blog, response: { kind: 'dismiss' } });
});

test('schedules panel: add a schedule', async ({ page }) => {
  await start(page, '01JAS000000000000000000018');
  const panel = page.getByLabel('Schedules');
  await expect(panel.getByText('Weekdays 09:00')).toBeVisible();
  await panel.getByRole('button', { name: '+ Schedule' }).click();
  await panel.getByLabel('Schedule name').fill('Evening sweep');
  await panel.getByLabel('Cron expression').fill('0 18 * * *');
  await panel.getByLabel('Schedule message').fill('Summarize the day');
  await panel.getByRole('button', { name: 'Save' }).click();
  const [call] = await calls(page, 'schedules.create');
  expect(call.params).toMatchObject({ name: 'Evening sweep', spec: { kind: 'cron', expr: '0 18 * * *' }, message: 'Summarize the day' });
});

test('schedules panel: recent runs and run now', async ({ page }) => {
  await start(page, '01JAS000000000000000000018');
  const row = page.getByLabel('Schedules').locator('[data-schedule="01JAC000000000000000000001"]');
  await row.getByRole('button', { name: 'fired' }).click();
  await expect(row.locator('.hist')).toContainText('missed \u00d73');
  await expect(row.locator('.hist')).toContainText('devbox unreachable');
  await row.getByRole('button', { name: 'Run now' }).click();
  const [call] = await calls(page, 'schedules.run_now');
  expect(call.params).toEqual({ schedule_id: '01JAC000000000000000000001' });
});

test('a question with several parts is answered in one go', async ({ page }) => {
  await start(page, S.blog);
  const card = page.getByRole('main').locator('[data-request="req-12"]');
  await expect(card.getByText('Which sections should it cover?')).toBeVisible();
  await card.getByRole('button', { name: 'Operators' }).click();
  await card.getByRole('button', { name: 'Setup' }).click();
  await card.getByRole('button', { name: 'Benchmarks' }).click();
  await card.getByRole('button', { name: 'Answer' }).click();
  const [call] = await calls(page, 'request.respond');
  expect(call.params).toMatchObject({
    id: S.blog,
    request_id: 'req-12',
    response: {
      kind: 'answers',
      answers: [
        { question_id: 'q1', selected: ['Operators'], text: null },
        { question_id: 'q2', selected: ['Setup', 'Benchmarks'], text: null },
      ],
    },
  });
});

test('session header: every workspace with its availability, and the overlap warning', async ({ page }) => {
  await start(page, S.auth);
  const header = page.getByRole('banner');
  const workspaces = header.getByLabel('Workspaces');
  await expect(workspaces.locator('[data-workspace]')).toHaveText([/buildbox:~\/src\/acme-api/, /devbox:~\/src\/acme-web/]);
  // Another open session works in the same directory: a warning, with a link to it.
  await expect(workspaces.locator('[data-overlap]')).toContainText('shared with');
  await expect(workspaces.locator('[data-overlap]')).toHaveAttribute('title', /^buildbox:~\/src\/acme-api/);
  await workspaces.locator('[data-overlap]').getByRole('button', { name: 'upgrade tokio to 1.47' }).click();
  await expect(page.getByRole('option', { name: /upgrade tokio to 1.47/ })).toHaveAttribute('aria-selected', 'true');

  // A workspace on an unreachable machine: the session keeps its state, the workspace waits.
  await start(page, S.dedupe);
  await expect(page.getByRole('banner').locator('[data-workspace]').first()).toContainText(/gpubox:~\/data\/dedupe\s*unreachable since/);
  await expect(page.locator('[data-machine-banner="gpubox"]')).toHaveAttribute('title', 'Tool calls there wait up to 10m');
  await expect(page.getByRole('banner').getByText('Needs approval')).toBeVisible();
});

test('a command waiting for terminal input names its shell; a privileged command says it runs unsandboxed', async ({ page }) => {
  await start(page, S.jobs);
  const input = page.locator('[data-request="req-term-1"]').first();
  await expect(input).toContainText('git rebase -i HEAD~3');
  await expect(input.locator('[data-shell="buildbox"]')).toContainText('type in its terminal');

  await start(page, S.deploy);
  const privileged = page.locator('[data-request="req-priv-1"]').first();
  await expect(privileged).toContainText('sudo systemctl restart nginx');
  await expect(privileged).toContainText('unsandboxed on devbox');
  await privileged.getByRole('button', { name: 'Allow once' }).click();
  const [call] = await calls(page, 'request.respond');
  expect(call.params).toMatchObject({ id: S.deploy, request_id: 'req-priv-1', response: { kind: 'choice', option_id: 'allow-once' } });
});
