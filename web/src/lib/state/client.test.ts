import { describe, expect, it } from 'vitest';
import { Listeners, NotificationHub, type AuthState, type ConnState, type Params, type Result, type Transport } from '../api/transport';
import type { StateSubscribeResult, UiMethod, UiNotificationName, UiNotifications } from '../protocol';
import { loadFixture } from '../../test/fixture';
import { Client } from './client.svelte';

/** A scriptable transport: records calls and answers from `answers`. */
class FakeTransport implements Transport {
  state: ConnState = 'connecting';
  auth: AuthState = { status: 'ok' };
  readonly calls: { method: UiMethod; params: unknown }[] = [];
  readonly hub = new NotificationHub();
  readonly states = new Listeners<ConnState>();
  subscribeResult: () => StateSubscribeResult = () => {
    throw new Error('unset');
  };

  request<M extends UiMethod>(method: M, params: Params<M>): Promise<Result<M>> {
    this.calls.push({ method, params });
    if (this.state !== 'open') return Promise.reject(new Error('offline'));
    const f = loadFixture();
    const answers: Partial<Record<UiMethod, () => unknown>> = {
      hello: () => f.hello,
      'devices.register': () => f.snapshot.devices[0],
      'state.subscribe': () => this.subscribeResult(),
      unsubscribe: () => ({}),
    };
    return Promise.resolve(answers[method]?.() as Result<M>);
  }
  subscribe<N extends UiNotificationName>(name: N, handler: (p: UiNotifications[N]) => void) {
    return this.hub.subscribe(name, handler);
  }
  onState(h: (s: ConnState) => void) {
    return this.states.add(h);
  }
  onAuth() {
    return () => {};
  }
  set(state: ConnState) {
    this.state = state;
    this.states.emit(state);
  }
  async logout() {}
  async openStream(): Promise<never> {
    throw new Error('unsupported');
  }
  resourceUrl() {
    return '';
  }
  close() {}
}

const settle = () => new Promise((r) => setTimeout(r, 0));
const device = { id: 'dev-1', label: 'Firefox on Linux', named: false };

describe('Client sync', () => {
  it('says hello, registers the device and loads the snapshot on open', async () => {
    const f = loadFixture();
    const t = new FakeTransport();
    t.subscribeResult = () => ({ subscription_id: 'sub-1', cursor: f.cursor, snapshot: f.snapshot });
    const client = new Client(t, device);
    t.set('open');
    await settle();
    expect(t.calls.map((c) => c.method)).toEqual(['hello', 'devices.register', 'state.subscribe']);
    expect(t.calls[0].params).toMatchObject({ protocol_version: { major: 1 }, device_id: 'dev-1' });
    expect(client.everConnected).toBe(true);
    expect(client.online).toBe(true);
    expect(client.feed.sessions.size).toBe(f.snapshot.sessions.length);
  });

  it('resubscribes from the last cursor after a reconnect and drops stale subscriptions', async () => {
    const f = loadFixture();
    const t = new FakeTransport();
    let n = 0;
    t.subscribeResult = () => (++n === 1 ? { subscription_id: 'sub-1', cursor: f.cursor, snapshot: f.snapshot } : { subscription_id: 'sub-2', cursor: f.cursor, snapshot: null });
    const client = new Client(t, device);
    t.set('open');
    await settle();
    t.set('closed');
    expect(client.online).toBe(false);
    expect(client.offlineSince).not.toBeNull();
    t.set('open');
    await settle();
    const subscribes = t.calls.filter((c) => c.method === 'state.subscribe');
    expect(subscribes[1].params).toEqual({ since: f.cursor });
    expect(client.feed.sessions.size).toBe(f.snapshot.sessions.length);
    // events of the old subscription are ignored; the new one applies
    const project = f.snapshot.projects[0];
    t.hub.emit('state.events', { subscription_id: 'sub-1', changes: [{ rev: f.cursor.rev + 1, kind: 'project_removed', project_id: project.id }] });
    expect(client.feed.projects.has(project.id)).toBe(true);
    t.hub.emit('state.events', { subscription_id: 'sub-2', changes: [{ rev: f.cursor.rev + 1, kind: 'project_removed', project_id: project.id }] });
    expect(client.feed.projects.has(project.id)).toBe(false);
  });

  it('resubscribes when the feed skips a revision', async () => {
    const f = loadFixture();
    const t = new FakeTransport();
    t.subscribeResult = () => ({ subscription_id: 'sub-1', cursor: f.cursor, snapshot: f.snapshot });
    new Client(t, device);
    t.set('open');
    await settle();
    t.hub.emit('state.events', { subscription_id: 'sub-1', changes: [{ rev: f.cursor.rev + 5, kind: 'unknown' }] });
    await settle();
    expect(t.calls.filter((c) => c.method === 'state.subscribe')).toHaveLength(2);
  });

  it('reports failed calls once and resolves them to undefined', async () => {
    const t = new FakeTransport();
    const client = new Client(t, device);
    const errors: string[] = [];
    client.onError = (m) => errors.push(m);
    await expect(client.call('keys.list', {})).resolves.toBeUndefined();
    expect(errors).toEqual(['offline']);
  });
});
