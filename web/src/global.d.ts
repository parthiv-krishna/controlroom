import type { FixtureTransport } from './lib/api/fixture';

declare global {
  interface Window {
    /** The fixture build's transport, for the Playwright smoke tests (absent in production). */
    __fixture?: FixtureTransport;
  }
}
