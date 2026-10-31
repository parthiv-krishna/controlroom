import { defineConfig, devices } from '@playwright/test';

// Smoke tests against the fixture build (`pnpm build:fixture`), served by vite preview.
// Run them with scripts/e2e.sh, which pins the browsers.
export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  fullyParallel: true,
  reporter: [['list']],
  use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 900 }, baseURL: 'http://127.0.0.1:4194/', trace: 'retain-on-failure' },
  webServer: {
    command: 'pnpm exec vite preview --mode fixture --outDir dist-fixture --port 4194 --strictPort --host 127.0.0.1',
    url: 'http://127.0.0.1:4194/',
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
