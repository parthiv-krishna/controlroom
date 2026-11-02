import { defineConfig, devices } from '@playwright/test';

// Specs against the real controlroom-web inside the NixOS VM test (nix/tests/e2e.nix), which runs
// one spec at a time and serves its steps (see helpers.ts). Settings come from E2E_* variables:
// E2E_BASE_URL, E2E_TOKEN_FILE (the dev token), E2E_GATES (step files), E2E_OUTPUT (traces).
export default defineConfig({
  testDir: '.',
  // A scenario includes SSH bootstraps, pi starts under nono and a network timeout.
  timeout: 10 * 60_000,
  expect: { timeout: 60_000 },
  workers: 1,
  retries: 0,
  reporter: [['list']],
  outputDir: process.env.E2E_OUTPUT ?? 'test-results',
  use: {
    ...devices['Desktop Chrome'],
    viewport: { width: 1440, height: 900 },
    baseURL: process.env.E2E_BASE_URL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    actionTimeout: 60_000,
  },
});
