import { expect, test, type Page, type WebSocketRoute } from '@playwright/test';
import fixture from '../fixtures/controlroom.json' with { type: 'json' };
import { S } from './helpers';

// The real WebSocket transport (`?ws`) against a mocked server socket: a dropped connection shows
// the one offline indicator, never a "Connection lost" toast, and resyncs quietly (cut-off calls
// that are safe to repeat go out again); a page coming back reconnects at once.

interface Call {
  id: number;
  method: string;
  params: Record<string, unknown>;
  /** The connection it came on, from 0. */
  socket: number;
}

const EMPTY_DETAIL = { agent_sessions: [], queue: [], todos: [], auto_continue: { status: 'off', reason: null, runs_used: 0, budget: 0 }, jobs: [], peers: [] };

/** The web server's side of `auth/session` and `api/ws`, answering from the committed fixture. */
class Server {
  readonly sockets: WebSocketRoute[] = [];
  readonly calls: Call[] = [];
  /** Methods left unanswered (in flight). */
  held = new Set<string>();
  /** Whether `auth/session` answers (a down server refuses the connection). */
  up = true;
  /** Connection attempts refused while down. */
  refused = 0;
  private subs = 0;

  async install(page: Page): Promise<void> {
    await page.route('**/auth/session', (route) => {
      if (this.up) return route.fulfill({ json: { csrf_token: 'c1' } });
      this.refused++;
      return route.abort('connectionrefused');
    });
    await page.routeWebSocket(/\/api\/ws$/, (ws) => {
      const socket = this.sockets.push(ws) - 1;
      ws.onMessage((raw) => {
        const msg = JSON.parse(String(raw)) as Omit<Call, 'socket'>;
        this.calls.push({ ...msg, socket });
        if (!this.held.has(msg.method)) ws.send(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: this.result(msg) }));
      });
    });
  }

  /** The methods of the calls on connection `socket`. */
  methods(socket: number): string[] {
    return this.calls.filter((c) => c.socket === socket).map((c) => c.method);
  }

  /** Drops the current connection, as a network change or the OS does. */
  drop(): void {
    void this.sockets.at(-1)?.close();
  }

  private result(msg: Omit<Call, 'socket'>): unknown {
    const responses = fixture.responses as Record<string, unknown>;
    switch (msg.method) {
      case 'hello':
        return fixture.hello;
      case 'state.subscribe':
        return { subscription_id: `state-${++this.subs}`, cursor: fixture.cursor, snapshot: fixture.snapshot };
      case 'session.subscribe': {
        const details = fixture.details as Record<string, unknown>;
        return { subscription_id: `session-${++this.subs}`, head_seq: 0, detail: details[msg.params.id as string] ?? EMPTY_DETAIL };
      }
      default:
        return responses[msg.method] ?? {};
    }
  }
}

const indicator = (page: Page) => page.getByRole('status').filter({ hasText: "Can't reach server" });
const toasts = (page: Page) => page.locator('.toasts > *');

async function open(page: Page, server: Server, query: string, session?: string): Promise<void> {
  await server.install(page);
  await page.goto(`/?${query}${session ? `#/s/${session}` : ''}`);
  await expect(page.getByRole('listbox', { name: 'Session list' })).toBeVisible();
}

test('a dropped connection: one offline indicator, no toasts, then a quiet resync', async ({ page }) => {
  const server = new Server();
  await open(page, server, 'ws', S.blog);
  await expect.poll(() => server.methods(0)).toContain('session.subscribe');

  // In flight when the connection drops: a prompt (an op_id command) and a read.
  server.held = new Set(['session.prompt', 'models.list']);
  const composer = page.getByRole('textbox', { name: 'Message' });
  await composer.fill('ship it');
  await composer.press('Enter');
  await page.getByRole('button', { name: 'Settings' }).click();
  await page.getByRole('dialog', { name: 'Settings' }).getByRole('tab', { name: 'Models' }).click();
  await expect.poll(() => server.methods(0)).toEqual(expect.arrayContaining(['session.prompt', 'models.list']));
  const prompt = server.calls.find((c) => c.method === 'session.prompt')!;

  server.up = false;
  server.drop();
  await expect(indicator(page)).toHaveCount(1);
  await expect(indicator(page)).toBeVisible();
  // A few reconnect attempts fail meanwhile: still only the indicator.
  await expect.poll(() => server.refused, { timeout: 10_000 }).toBeGreaterThanOrEqual(2);
  await expect(toasts(page)).toHaveCount(0);

  server.held.clear();
  server.up = true;
  await expect(indicator(page)).toHaveCount(0, { timeout: 10_000 });
  // The resync: hello first, the feeds from their cursors, and the cut-off calls again (the
  // prompt under its op_id, so the server applies it once).
  await expect.poll(() => server.methods(1)).toEqual(expect.arrayContaining(['hello', 'devices.register', 'state.subscribe', 'session.subscribe', 'session.prompt', 'models.list']));
  expect(server.methods(1)[0]).toBe('hello');
  const again = server.calls.find((c) => c.socket === 1 && c.method === 'session.prompt')!;
  expect(again.params.op_id).toBe(prompt.params.op_id);
  expect(server.calls.find((c) => c.socket === 1 && c.method === 'state.subscribe')!.params.since).toEqual(fixture.cursor);
  await expect(toasts(page)).toHaveCount(0);
  await expect(page.getByText('Connection lost')).toHaveCount(0);
});

test('a page coming back reconnects at once instead of waiting out the backoff', async ({ page }) => {
  const server = new Server();
  // A minute's backoff: only the page's return can reconnect within the test.
  await open(page, server, 'ws&backoff=60000');
  server.drop();
  await expect(indicator(page)).toBeVisible();

  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'visible' });
    document.dispatchEvent(new Event('visibilitychange'));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
  });
  await expect(indicator(page)).toHaveCount(0);
  expect(server.sockets).toHaveLength(2);
  await expect.poll(() => server.methods(1)).toEqual(expect.arrayContaining(['hello', 'state.subscribe']));
  await expect(toasts(page)).toHaveCount(0);
});
