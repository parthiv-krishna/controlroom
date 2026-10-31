import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// base './' keeps asset paths relative so the dist works under rust-embed.
// No CDN assets and no inline scripts (CSP script-src 'self').
export default defineConfig({
  base: './',
  plugins: [svelte()],
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: true, target: 'es2022', assetsInlineLimit: 0 },
  server: { host: '127.0.0.1', port: 5173 },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
});
