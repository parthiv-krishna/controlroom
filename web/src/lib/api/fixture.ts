// A transport for development and tests that serves recorded data: a `state.subscribe` snapshot,
// session details, canned query results and scripted feed events from a JSON fixture
// (web/fixtures/). It holds no backend logic: mutations are only recorded (and answered with an
// ack, or a canned result when the fixture has one), and the feed changes only by script.
import type {
  OnMachine,
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

/** One scripted feed event; the transport fills in the subscription id it handed out. */
export type FixtureStep =
  | { delay_ms: number; notification: 'state.events'; params: Omit<StateEventsParams, 'subscription_id'> }
  | { delay_ms: number; notification: 'session.events'; params: Omit<OnMachine<SessionEventsParams>, 'subscription_id'> };

export interface Fixture {
  hello: UiHelloResult;
  cursor: StateCursor;
  snapshot: StateSnapshot;
  /** Session details by `<machine id>/<session id>`. */
  details: Record<string, SessionDetail>;
  /** Canned results of query methods (and of mutations whose result isn't an ack). */
  responses: { [M in UiMethod]?: Result<M> };
  script: FixtureStep[];
}

export interface RecordedCall {
  method: UiMethod;
  params: unknown;
}

export interface FixtureOptions {
  /** Never connect: shows the "can't reach server" state. */
  offline?: boolean;
  /** Play the script on its timers after `state.subscribe` (default true); otherwise use step(). */
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
  private readonly sessionSubs = new Map<string, string>();
  private stateSub: string | null = null;
  private next = 0;
  private played = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly fixture: Fixture,
    private readonly options: FixtureOptions = {},
  ) {
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
      const sub = this.sessionSubs.get(`${step.params.machine_id}/${step.params.id}`);
      if (sub) this.hub.emit('session.events', { ...step.params, subscription_id: sub });
    }
    return true;
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
        const { machine_id, id } = params as Params<'session.subscribe'>;
        const key = `${machine_id}/${id}`;
        const subscription_id = `fixture-session-${++this.next}`;
        this.sessionSubs.set(key, subscription_id);
        return { subscription_id, head_seq: 0, detail: this.fixture.details[key] ?? EMPTY_DETAIL };
      }
      case 'unsubscribe': {
        const { subscription_id } = params as Params<'unsubscribe'>;
        for (const [key, sub] of this.sessionSubs) if (sub === subscription_id) this.sessionSubs.delete(key);
        return {};
      }
      default:
        return undefined;
    }
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
