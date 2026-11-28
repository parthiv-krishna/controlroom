import { describe, expect, it } from 'vitest';
import { ConnectionError } from '../api/errors';
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
  /** A server before profiles doesn't announce them in its `hello`. */
  profiles = true;

  /** Fails the next calls (of any method) with this error. */
  failWith: Error | null = null;

  request<M extends UiMethod>(method: M, params: Params<M>): Promise<Result<M>> {
    this.calls.push({ method, params });
    if (this.failWith) return Promise.reject(this.failWith);
    if (this.state !== 'open') return Promise.reject(new Error('offline'));
    const f = loadFixture();
    const answers: Partial<Record<UiMethod, () => unknown>> = {
      hello: () => (this.profiles ? f.hello : { ...f.hello, capabilities: f.hello.capabilities.filter((c) => c !== 'profiles') }),
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
  openTerminal(): never {
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
    expect(client.profilesSupported).toBe(true);
    expect(client.feed.profileList.map((p) => p.id)).toEqual(f.snapshot.profiles.map((p) => p.id));
  });

  it('knows a server before profiles by its hello', async () => {
    const f = loadFixture();
    const t = new FakeTransport();
    t.profiles = false;
    t.subscribeResult = () => ({ subscription_id: 'sub-1', cursor: f.cursor, snapshot: f.snapshot });
    const client = new Client(t, device);
    t.set('open');
    await settle();
    expect(client.everConnected).toBe(true);
    expect(client.profilesSupported).toBe(false);
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

  it('leaves the lost connection to the offline indicator: reads, sign-outs and reports are silent', async () => {
    const t = new FakeTransport();
    const client = new Client(t, device);
    const errors: string[] = [];
    client.onError = (m) => errors.push(m);
    t.failWith = new ConnectionError('offline');
    await client.call('models.list', {});
    client.fire('client.focus', { device_id: 'dev-1', session_id: null, visible: true });
    t.failWith = new ConnectionError('closed');
    await client.call('tasks.delete', { task_id: 't1' });
    await settle();
    expect(errors).toEqual([]);
    // A user's action that may not have gone through is reported, once per call.
    t.failWith = new ConnectionError('lost');
    await client.call('tasks.delete', { task_id: 't1' });
    expect(errors).toEqual(['Connection lost; it may not have gone through']);
  });

  it('a sync cut off by a drop stays silent and the next connection syncs again', async () => {
    const f = loadFixture();
    const t = new FakeTransport();
    const errors: string[] = [];
    let n = 0;
    t.subscribeResult = () => {
      if (++n === 1) throw new ConnectionError('lost');
      return { subscription_id: 'sub-2', cursor: f.cursor, snapshot: f.snapshot };
    };
    const client = new Client(t, device);
    client.onError = (m) => errors.push(m);
    t.set('open');
    await settle();
    expect(client.everConnected).toBe(false);
    t.set('closed');
    t.set('open');
    await settle();
    expect(client.everConnected).toBe(true);
    expect(errors).toEqual([]);
  });
});
