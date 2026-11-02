import { mount } from 'svelte';
import App from './App.svelte';
import type { Fixture } from './lib/api/fixture';
import type { Transport } from './lib/api/transport';
import { WebSocketTransport } from './lib/api/ws';
import { registerServiceWorker } from './lib/pwa';
import './styles/global.css';

/** The SPA root, where index.html, sw.js, api/ and auth/ live (the page may be a deep link). */
const base = new URL('./', location.href.split('#')[0].split('?')[0]).href;

/**
 * Production talks to the web server. `--mode fixture` (pnpm dev, the e2e build) serves the
 * committed fixture instead; that code is not in the production bundle. `?offline` keeps the
 * fixture build disconnected; the fixture transport is exposed for the smoke tests.
 */
async function pickTransport(): Promise<Transport> {
  if (import.meta.env.MODE === 'fixture') {
    const { FixtureTransport } = await import('./lib/api/fixture');
    const { default: fixture } = await import('../fixtures/controlroom.json');
    const { default: timelines } = await import('../fixtures/timelines.json');
    const query = new URLSearchParams(location.search);
    // SAFETY: JSON imports widen string unions; fixture.test.ts checks the fixture's consistency.
    const data = { ...fixture, timelines } as unknown as Fixture;
    const transport = new FixtureTransport(data, { offline: query.has('offline'), autoplay: !query.has('paused') });
    window.__fixture = transport;
    return transport;
  }
  return new WebSocketTransport({ base });
}

const target = document.getElementById('app');
if (!target) throw new Error('missing #app');
void pickTransport().then((transport) => mount(App, { target, props: { transport } }));
if (import.meta.env.PROD && import.meta.env.MODE !== 'fixture') void registerServiceWorker(base);
