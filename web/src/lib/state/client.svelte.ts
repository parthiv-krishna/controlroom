// The sync engine: on every (re)connect it says hello, registers this device, subscribes to the
// state feed from the last cursor and resumes the open session feeds. Calls go through `call`
// (a user's action: a failure is reported through `onError` and resolves to undefined) or
// `fire` (a background report: failures stay silent), typed by UiMethods.
//
// The connection's state shows once, in the offline indicator: a read cut off by the connection
// is never reported (the transport sends it again after the reconnect, see api/policy.ts), and
// neither is a call refused because the user signed out (the login screen shows).
import { ConnectionError, describeError } from '../api/errors';
import { isRead } from '../api/policy';
import type { AuthState, CallOptions, ConnState, Params, Result, Transport } from '../api/transport';
import { UI_PROTOCOL_VERSION, type DeviceRef, type UiHelloResult, type UiMethod } from '../protocol';
import { rememberLabel, type LocalDevice } from './device';
import { StateFeed } from './feed.svelte';
import { SessionFeeds } from './session-feeds.svelte';

export class Client {
  conn = $state<ConnState>('connecting');
  auth = $state<AuthState>({ status: 'unknown' });
  /** False until the first successful sync: the "can't reach server" screen shows until then. */
  everConnected = $state(false);
  offlineSince = $state<number | null>(null);
  hello = $state.raw<UiHelloResult | null>(null);
  /** This browser's device; `named` drives the first-load naming prompt. */
  device = $state<LocalDevice>({ id: '', label: '', named: true });
  readonly feed = new StateFeed();
  readonly sessions: SessionFeeds;
  /** Reports a failed call (a toast). */
  onError: (message: string) => void = () => {};

  private stateSub: string | null = null;
  private syncing: Promise<void> | null = null;

  constructor(
    readonly transport: Transport,
    device: LocalDevice,
  ) {
    this.device = device;
    this.sessions = new SessionFeeds({
      subscribe: (id, since_seq, raw_since) => this.transport.request('session.subscribe', { id, since_seq, raw_since }),
      unsubscribe: (subscription_id) => this.transport.request('unsubscribe', { subscription_id }),
    });
    this.conn = transport.state;
    this.auth = transport.auth;
    transport.onAuth((a) => (this.auth = a));
    transport.onState((s) => this.onState(s));
    transport.subscribe('state.events', (p) => {
      if (p.subscription_id !== this.stateSub) return;
      // A gap resubscribes (a drop meanwhile is the next sync's).
      if (this.feed.apply(p.changes) === 'gap') this.syncState().catch(() => {});
      // A removed session's timeline is never shown again.
      for (const c of p.changes) if (c.kind === 'session_removed') this.sessions.drop(c.session_id);
    });
    transport.subscribe('session.events', (p) => this.sessions.receive(p));
    if (transport.state === 'open') void this.sync();
    else if (transport.state === 'closed') this.offlineSince = Date.now();
  }

  /**
   * Whether the server has profiles (its `hello` announces `profiles`). Without them nothing asks
   * for a profile first, and nothing sends one.
   */
  get profilesSupported(): boolean {
    return this.hello?.capabilities.includes('profiles') ?? false;
  }

  /** Whether calls can go out now; actions are disabled otherwise. */
  get online(): boolean {
    return this.conn === 'open' && this.everConnected;
  }

  /** This device as attribution (prompts, answers). */
  get deviceRef(): DeviceRef {
    return { device_id: this.device.id, label: this.serverLabel() };
  }

  /** A user's action; a failure is reported through onError and resolves to undefined. */
  async call<M extends UiMethod>(method: M, params: Params<M>, options?: CallOptions): Promise<Result<M> | undefined> {
    try {
      return await this.transport.request(method, params, options);
    } catch (e) {
      this.report(method, e);
      return undefined;
    }
  }

  /** A background report (focus, presence, read cursors): fire and forget, failures stay silent. */
  fire<M extends UiMethod>(method: M, params: Params<M>, options?: CallOptions): void {
    this.transport.request(method, params, options).catch(() => {});
  }

  /** A call whose error the caller handles itself (wizards, inline errors). */
  request<M extends UiMethod>(method: M, params: Params<M>, options?: CallOptions): Promise<Result<M>> {
    return this.transport.request(method, params, options);
  }

  /** Names this device (the first-load prompt, or Settings › Devices for this device). */
  async nameDevice(label: string): Promise<void> {
    rememberLabel(label);
    this.device = { ...this.device, label, named: true };
    await this.call('devices.rename', { device_id: this.device.id, label });
  }

  /** Keeps the guessed label and hides the prompt. */
  skipNaming(): void {
    rememberLabel(this.device.label);
    this.device = { ...this.device, named: true };
  }

  private serverLabel(): string {
    return this.feed.devices.get(this.device.id)?.label ?? this.device.label;
  }

  /** Reports a failed call, unless the offline indicator or the login screen already says it. */
  private report(method: UiMethod, e: unknown): void {
    if (e instanceof ConnectionError && (isRead(method) || e.reason === 'closed')) return;
    this.onError(describeError(e));
  }

  private onState(state: ConnState): void {
    this.conn = state;
    if (state === 'open') void this.sync();
    else if (state === 'closed' && this.offlineSince === null) this.offlineSince = Date.now();
  }

  /** Syncs on the current connection, after a sync still running on an earlier one. */
  private sync(): Promise<void> {
    const run = (this.syncing ?? Promise.resolve()).then(() => (this.transport.state === 'open' ? this.runSync() : undefined));
    const syncing = run.finally(() => {
      if (this.syncing === syncing) this.syncing = null;
    });
    this.syncing = syncing;
    return syncing;
  }

  private async runSync(): Promise<void> {
    try {
      this.hello = await this.transport.request('hello', {
        protocol_version: UI_PROTOCOL_VERSION,
        client_name: typeof navigator === 'undefined' ? null : navigator.userAgent,
        device_id: this.device.id,
      });
      await this.transport.request('devices.register', { device_id: this.device.id, label: this.device.label });
      await this.syncState();
      this.sessions.resubscribeAll();
      this.everConnected = true;
      this.offlineSince = null;
    } catch (e) {
      // The connection dropped (the next `open` syncs again), or the server refused.
      if (!(e instanceof ConnectionError) && this.transport.state === 'open') this.onError(describeError(e));
    }
  }

  /** Subscribes to the state feed from the last cursor; a snapshot replaces everything. */
  private async syncState(): Promise<void> {
    const r = await this.transport.request('state.subscribe', { since: this.feed.cursor });
    const old = this.stateSub;
    this.stateSub = r.subscription_id;
    if (old && old !== r.subscription_id) void this.transport.request('unsubscribe', { subscription_id: old }).catch(() => {});
    if (r.snapshot) this.feed.reset(r.snapshot, r.cursor);
    else this.feed.resume(r.cursor);
  }
}
