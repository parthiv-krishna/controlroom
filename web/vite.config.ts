import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { defineConfig, type Plugin } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Emits `<root>/sw.js` from `sw/sw.js` with the app-shell precache list and a cache version
 * derived from the content of every precached file, so any change (public/ included) installs a
 * new worker. Source maps are never precached.
 */
function serviceWorker(): Plugin {
  return {
    name: 'controlroom-sw',
    apply: 'build',
    enforce: 'post',
    generateBundle(_options, bundle) {
      const hash = createHash('sha256');
      const files: string[] = [];
      for (const name of Object.keys(bundle).sort()) {
        if (name.endsWith('.map') || name === 'sw.js') continue;
        const chunk = bundle[name];
        files.push(name);
        hash.update(name).update(chunk.type === 'chunk' ? chunk.code : chunk.source);
      }
      const source = readFileSync(new URL('./sw/sw.js', import.meta.url), 'utf8')
        .replace('self.__PRECACHE__', JSON.stringify(['./', ...files]))
        .replace('self.__VERSION__', JSON.stringify(hash.digest('hex').slice(0, 16)));
      this.emitFile({ type: 'asset', fileName: 'sw.js', source });
    },
  };
}

// base './' keeps asset paths relative so the dist works under rust-embed at any prefix.
// No CDN assets and no inline scripts (CSP script-src 'self').
// `--mode fixture` builds the fixture-transport variant used by `pnpm dev` and the Playwright
// smoke tests; the production build contains no fixture code.
export default defineConfig(({ mode }) => ({
  base: './',
  plugins: [svelte(), serviceWorker()],
  build: {
    outDir: mode === 'fixture' ? 'dist-fixture' : 'dist',
    emptyOutDir: true,
    sourcemap: true,
    target: 'es2022',
    assetsInlineLimit: 0,
  },
  server: { host: '127.0.0.1', port: 5173 },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
  resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
}));
