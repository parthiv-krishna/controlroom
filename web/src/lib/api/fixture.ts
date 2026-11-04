// A transport for development and tests that serves recorded data: a `state.subscribe` snapshot,
// session details, canned query results and scripted feed events from a JSON fixture
// (web/fixtures/), plus recorded session timelines (see fixture-timelines.ts). It holds no backend
// logic: mutations are only recorded (and answered with an ack, or a canned result when the
// fixture has one), and the feeds change only by script.
import type {
  SessionDetail,
  SessionEventsParams,
  StateCursor,
  StateEventsParams,
  StateSnapshot,
  UiHelloResult,
  UiMethod,
  UiMethods,
  UiNotificationName,
  UiNotifications,
} from '../protocol';
import { ConnectionError, RpcError } from './errors';
import {
  Listeners,
  NotificationHub,
  fileUrl,
  streamsNotSupported,
  type AuthState,
  type ConnState,
  type DuplexStream,
  type Params,
  type Result,
  type Transport,
} from './transport';
import { TimelineReplay, type FixtureTimeline } from './fixture-timelines';

/** One scripted feed event; the transport fills in the subscription id it handed out. */
export type FixtureStep =
  | { delay_ms: number; notification: 'state.events'; params: Omit<StateEventsParams, 'subscription_id'> }
  | { delay_ms: number; notification: 'session.events'; params: Omit<SessionEventsParams, 'subscription_id'> };

export interface Fixture {
  hello: UiHelloResult;
  cursor: StateCursor;
  snapshot: StateSnapshot;
  /** Session details by session id. */
  details: Record<string, SessionDetail>;
  /** Canned results of query methods (and of mutations whose result isn't an ack). */
  responses: { [M in UiMethod]?: Result<M> };
  script: FixtureStep[];
  /** Recorded timelines by session id (web/fixtures/timelines.json). */
  timelines?: Record<string, FixtureTimeline>;
}

export interface RecordedCall {
  method: UiMethod;
  params: unknown;
}

export interface FixtureOptions {
  /** Never connect: shows the "can't reach server" state. */
  offline?: boolean;
  /**
   * Play the script on its timers after `state.subscribe`, and a session's live timeline steps
   * after its first subscribe (default true); otherwise use step() and advance().
   */
  autoplay?: boolean;
}

const EMPTY_DETAIL: SessionDetail = {
  agent_sessions: [],
  queue: [],
  todos: [],
  auto_continue: { status: 'off', reason: null, runs_used: 0, budget: 0 },
  jobs: [],
  peers: [],
};

type AckMethod = { [M in UiMethod]: UiMethods[M]['result'] extends Record<symbol, never> ? M : never }[UiMethod];

export class FixtureTransport implements Transport {
  state: ConnState = 'connecting';
  auth: AuthState = { status: 'ok' };
  /** Every call, in order (inspected by tests). */
  readonly calls: RecordedCall[] = [];

  private readonly hub = new NotificationHub();
  private readonly stateListeners = new Listeners<ConnState>();
  /** Session subscriptions by session id, with their `raw_since`. */
  private readonly sessionSubs = new Map<string, { id: string; rawSince: number | null }>();
  private readonly replays: Map<string, TimelineReplay>;
  private readonly liveTimers = new Map<string, ReturnType<typeof setTimeout> | null>();
  private stateSub: string | null = null;
  private next = 0;
  private played = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly fixture: Fixture,
    private readonly options: FixtureOptions = {},
  ) {
    this.replays = new Map(Object.entries(fixture.timelines ?? {}).map(([key, t]) => [key, new TimelineReplay(t)]));
    if (options.offline) this.state = 'closed';
    else queueMicrotask(() => this.setState('open'));
  }

  request<M extends UiMethod>(method: M, params: Params<M>): Promise<Result<M>> {
    if (this.state !== 'open') return Promise.reject(new ConnectionError('offline'));
    this.calls.push({ method, params });
    return Promise.resolve().then(() => this.answer(method, params));
  }

  subscribe<N extends UiNotificationName>(name: N, handler: (params: UiNotifications[N]) => void): () => void {
    return this.hub.subscribe(name, handler);
  }
  onState(handler: (state: ConnState) => void): () => void {
    return this.stateListeners.add(handler);
  }
  onAuth(): () => void {
    return () => {};
  }
  async logout(): Promise<void> {}
  openStream(): Promise<DuplexStream> {
    return streamsNotSupported();
  }
  resourceUrl(machineId: string, path: string): string {
    return fileUrl('./', machineId, path);
  }
  close(): void {
    if (this.timer) clearTimeout(this.timer);
    for (const t of this.liveTimers.values()) if (t) clearTimeout(t);
    this.setState('closed');
  }

  /** Emits the next scripted event now; false when the script is done. */
  step(): boolean {
    const step = this.fixture.script[this.played];
    if (!step) return false;
    this.played++;
    if (step.notification === 'state.events') {
      if (this.stateSub) this.hub.emit('state.events', { ...step.params, subscription_id: this.stateSub });
    } else {
      const sub = this.sessionSubs.get(step.params.id);
      if (sub) this.hub.emit('session.events', { ...step.params, subscription_id: sub.id });
    }
    return true;
  }

  /** Plays a session's next `n` live timeline steps now; returns how many there were. */
  advance(id: string, n = 1): number {
    let done = 0;
    while (done < n && this.playLive(id)) done++;
    return done;
  }

  // ---------------------------------------------------------------------------------------------

  private setState(state: ConnState): void {
    if (this.state === state) return;
    this.state = state;
    this.stateListeners.emit(state);
  }

  private answer<M extends UiMethod>(method: M, params: Params<M>): Result<M> {
    const result = this.subscription(method, params) ?? this.fixture.responses[method] ?? (isAck(method) ? {} : undefined);
    if (result === undefined) throw new RpcError(-32006, `fixture: no result for ${method}`, { kind: 'unsupported' });
    return result as Result<M>;
  }

  /** The subscription methods: they hand out the ids that the scripted events refer to. */
  private subscription(method: UiMethod, params: Params<UiMethod>): Result<UiMethod> | undefined {
    switch (method) {
      case 'hello':
        return this.fixture.hello;
      case 'state.subscribe': {
        this.stateSub = `fixture-state-${++this.next}`;
        if (this.options.autoplay ?? true) this.playNext();
        return { subscription_id: this.stateSub, cursor: this.fixture.cursor, snapshot: this.fixture.snapshot };
      }
      case 'session.subscribe': {
        const { id, since_seq, raw_since } = params as Params<'session.subscribe'>;
        const key = id;
        const sub = { id: `fixture-session-${++this.next}`, rawSince: raw_since ?? null };
        this.sessionSubs.set(key, sub);
        const replay = this.replays.get(key);
        if (replay) {
          // After the result, like the server: the backlog, then live steps.
          setTimeout(() => this.sendBacklog(id, sub, since_seq), 0);
          if ((this.options.autoplay ?? true) && !this.liveTimers.has(key)) this.scheduleLive(key);
        }
        return { subscription_id: sub.id, head_seq: replay?.head ?? 0, detail: this.fixture.details[key] ?? EMPTY_DETAIL };
      }
      case 'unsubscribe': {
        const { subscription_id } = params as Params<'unsubscribe'>;
        for (const [key, sub] of this.sessionSubs) if (sub.id === subscription_id) this.sessionSubs.delete(key);
        return {};
      }
      default:
        return undefined;
    }
  }

  private sendBacklog(id: string, sub: { id: string; rawSince: number | null }, since: number): void {
    const replay = this.replays.get(id);
    if (!replay || this.sessionSubs.get(id) !== sub) return;
    const { ops, raw } = replay.backlog(since, sub.rawSince);
    if (ops.length || raw.length) this.hub.emit('session.events', { subscription_id: sub.id, id, head_seq: replay.head, ops, raw });
  }

  /** Plays one live step of a session's timeline (sent if it is subscribed). */
  private playLive(key: string): boolean {
    const step = this.replays.get(key)?.take();
    if (!step) return false;
    const sub = this.sessionSubs.get(key);
    if (sub) {
      const raw = sub.rawSince === null ? [] : step.raw.filter((f) => f.seq > sub.rawSince!);
      this.hub.emit('session.events', { subscription_id: sub.id, id: key, head_seq: step.head_seq, ops: step.ops, raw });
    }
    return true;
  }

  private scheduleLive(key: string): void {
    const step = this.replays.get(key)?.peek();
    if (!step) return;
    this.liveTimers.set(
      key,
      setTimeout(() => {
        this.playLive(key);
        this.scheduleLive(key);
      }, step.delay_ms),
    );
  }

  private playNext(): void {
    const step = this.fixture.script[this.played];
    if (!step || this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.step();
      this.playNext();
    }, step.delay_ms);
  }
}

const ACK_METHODS = new Set<UiMethod>([
  'unsubscribe', 'machines.remove', 'machines.reconnect', 'machines.upgrade', 'keys.delete', 'projects.delete',
  'tasks.delete', 'grants.revoke', 'devices.remove', 'schedules.delete', 'client.focus', 'session.cancel',
  'session.close', 'session.reopen', 'session.resume', 'session.delete', 'session.update', 'session.set_option',
  'queue.edit', 'queue.remove', 'queue.reorder', 'queue.resume', 'todo.remove', 'todo.reorder', 'sandbox.revoke',
] satisfies AckMethod[]);
const isAck = (method: UiMethod): boolean => ACK_METHODS.has(method);
