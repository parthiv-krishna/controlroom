// The application context shared by all components: the client (server state), preferences, view
// state, overlays, the derived session listing, and the actions that carry UI policy (confirm
// before archiving a running session, keep archived rows until navigating away, …).
import { getContext, setContext } from 'svelte';
import { visibleSessions } from '../model/filters';
import { GROUPS, groupSessions, type Group, type GroupBy } from '../model/grouping';
import { deepLinkHash } from '../model/misc';
import { UNASSIGNED, type Ctx, type Scope } from '../model/projects';
import { busy } from '../model/status';
import { taskPrompt } from '../model/tasks';
import { randomUuid } from '../model/uuid';
import { leadsToMatch, treeIndex, treeRoots, type TreeIndex } from '../model/tree';
import type { FeedSession, OpId, Project, SessionDetail, SessionId, Task, TaskStatus } from '../protocol';
import type { Client } from '../state/client.svelte';
import { Dialogs, Toasts } from './overlays.svelte';
import { Prefs } from './prefs.svelte';
import { View, type NewSessionPreset, type SessionRef, type SettingsTab } from './view.svelte';

/** A fresh idempotency key for one user action. */
export const newOpId = (): OpId => randomUuid();

/** The common params of a pass-through command on a session. */
export const onSession = (s: SessionRef) => ({ op_id: newOpId(), id: s.id });

export class App {
  readonly prefs = new Prefs();
  readonly view = new View();
  readonly dialogs = new Dialogs();
  readonly toasts = new Toasts();

  readonly ctx: Ctx;
  /** The rail's scope. */
  readonly scope: Scope;
  /** The scope's project, when it is one (Unassigned and everything aren't). */
  readonly project: Project | null;
  readonly current: FeedSession | undefined;
  /** The session list as shown: visible sessions, their groups and the subagent tree. */
  readonly listing: { visible: Set<string>; tree: TreeIndex; groups: Group[] };

  constructor(readonly client: Client) {
    client.onError = (message) => this.toasts.show(message, 'error');
    const feed = client.feed;
    this.ctx = $derived({ machines: feed.machines, machineList: feed.machineList, sessions: feed.sessionList, tasks: feed.taskList, overlaps: feed.overlaps });
    this.project = $derived((this.view.project && this.view.project !== UNASSIGNED && feed.projects.get(this.view.project)) || null);
    this.scope = $derived(this.view.project === UNASSIGNED ? UNASSIGNED : this.project);
    this.current = $derived(this.view.current ? feed.sessions.get(this.view.current.id) : undefined);
    this.listing = $derived.by(() => {
      const keep = new Set(this.view.keep);
      const list = visibleSessions(this.ctx, this.scope, this.view.filters, this.view.q, keep);
      const visible = new Set(list.map((s) => s.id));
      const tree = treeIndex(this.ctx.sessions, keep);
      const groups = groupSessions(this.ctx, this.scope, list, this.prefs.group, this.view.filtering);
      return { visible, tree, groups };
    });
  }

  /** The detail of a session whose feed is held (the open session), else null. */
  detail(s: SessionRef): SessionDetail | null {
    return this.client.sessions.get(s.id)?.detail ?? null;
  }

  get desk(): boolean {
    return this.view.wide;
  }

  // -------------------------------------------------------------------------------- navigation

  open(ref: SessionRef): void {
    if (!this.view.keep.includes(ref.id)) this.view.keep = [];
    this.view.current = { id: ref.id };
    this.view.page = 'sessions';
    this.view.phoneDetail = true;
    this.view.drawer = false;
    if (typeof history !== 'undefined') history.replaceState(null, '', deepLinkHash(ref.id));
  }

  /** Scopes the views to a project, to Unassigned, or to everything (null). */
  openProject(id: string | null): void {
    this.view.keep = [];
    this.view.project = id;
    this.view.openTask = null;
    this.view.drawer = false;
    if (this.view.settings && this.prefs.settingsPage) this.view.settings = null;
  }

  openTask(id: string): void {
    this.view.openTask = id;
    if (this.desk) {
      if (!this.prefs.topPin) this.view.topPeek = true;
    } else this.view.page = 'tasks';
  }

  openSettings(tab: SettingsTab = 'machines'): void {
    this.view.drawer = false;
    this.view.settings = tab;
  }

  /** Sessions in list order (as shown, folded groups and subagents excluded), for j/k. */
  orderedSessions(): FeedSession[] {
    const { groups, visible, tree } = this.listing;
    const out: FeedSession[] = [];
    const seen = new Set<string>();
    const walk = (s: FeedSession, dim: boolean) => {
      const k = s.id;
      if (seen.has(k)) return;
      seen.add(k);
      if (!dim || visible.has(k)) out.push(s);
      if (this.view.collapsed['c:' + k]) return;
      for (const c of tree.children(s)) if (!dim || leadsToMatch(c, visible, tree)) walk(c, dim && !visible.has(c.id));
    };
    for (const g of groups) {
      if (this.view.collapsed[g.key]) continue;
      for (const r of treeRoots(g.items, visible, tree)) walk(r.s, r.dim);
    }
    return out;
  }

  move(step: number): void {
    const list = this.orderedSessions();
    if (!list.length) return;
    const cur = this.view.current;
    const i = cur ? list.findIndex((s) => s.id === cur.id) : -1;
    const next = i < 0 ? list[step > 0 ? 0 : list.length - 1] : list[Math.max(0, Math.min(list.length - 1, i + step))];
    this.open(next);
  }

  cycleGroup(): void {
    const order: GroupBy[] = GROUPS.map(([g]) => g);
    this.prefs.group = order[(order.indexOf(this.prefs.group) + 1) % order.length];
  }

  // ------------------------------------------------------------------------------------- panes

  toggleLeft(): void {
    this.prefs.leftPin = !this.prefs.leftPin;
    this.view.leftPeek = false;
  }
  toggleTop(): void {
    this.prefs.topPin = !this.prefs.topPin;
    this.view.topPeek = false;
  }
  toggleNotes(): void {
    if (!this.desk) {
      this.view.notesOpen = !this.view.notesOpen;
      return;
    }
    this.prefs.notesPin = !this.prefs.notesPin;
    this.view.notesOpen = false;
  }

  // ---------------------------------------------------------------------------------- creating

  newSession(preset: NewSessionPreset = {}): void {
    this.view.modal = { kind: 'new-session', preset };
  }

  startTask(t: Task): void {
    this.newSession({ tasks: [t.id], prompt: taskPrompt(t) });
  }

  /** The new-task dialog, pre-assigned to the project being viewed. */
  newTask(status: TaskStatus = 'todo', sessions: SessionId[] = []): void {
    this.view.modal = { kind: 'new-task', preset: { status, project_ids: this.project ? [this.project.id] : [], sessions } };
  }

  // ------------------------------------------------------------------------- session actions

  /** Archive (close); the row stays visible with ↺ until the user navigates away. */
  async archive(s: FeedSession): Promise<void> {
    if (busy(s) && !(await this.dialogs.confirm({ title: 'Stop and archive?', ok: 'Stop and archive', danger: true }))) return;
    if ((await this.client.call('session.close', onSession(s))) === undefined) return;
    if (!this.view.keep.includes(s.id)) this.view.keep = [...this.view.keep, s.id];
  }

  async unarchive(s: FeedSession): Promise<void> {
    try {
      await this.client.request('session.reopen', onSession(s));
    } catch (e) {
      const deleted = e instanceof Error && 'data' in e && (e as { data: { kind: string } }).data.kind === 'deleted';
      this.toasts.show(deleted ? 'That session was already deleted by retention' : 'Could not unarchive', 'error');
    }
  }

  async remove(s: FeedSession): Promise<void> {
    if (!(await this.dialogs.confirm({ title: 'Delete session?', body: 'The transcript is deleted permanently.', ok: 'Delete', danger: true }))) return;
    if ((await this.client.call('session.delete', onSession(s))) === undefined) return;
    if (this.view.current?.id === s.id) this.view.current = null;
  }

  /** Marks everything up to the last turn's end as read. */
  markRead(s: FeedSession): void {
    void this.client.call('session.mark_read', { ...onSession(s), seq: s.last_turn_end_seq });
  }

  /** Reports what this tab shows, for push deduplication. */
  reportFocus(): void {
    if (!this.client.online) return;
    const c = this.view.current;
    const showing = c && (this.desk || this.view.phoneDetail) && !this.view.settings;
    const visible = typeof document === 'undefined' || (document.visibilityState === 'visible' && document.hasFocus());
    void this.client.call('client.focus', {
      device_id: this.client.device.id,
      session_id: showing ? c.id : null,
      visible,
    });
  }

  async copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.toasts.show('Copied', 'ok');
    } catch {
      this.toasts.show("Couldn't copy", 'error');
    }
  }

}

const KEY = Symbol('controlroom-app');
export const provideApp = (app: App): App => setContext(KEY, app);
export const useApp = (): App => getContext<App>(KEY);
