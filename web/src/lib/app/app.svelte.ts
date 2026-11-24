// The application context shared by all components: the client (server state), preferences, view
// state, overlays, the current profile and its data (`ctx`, the one scoped context every view
// reads), the derived session listing, and the actions that carry UI policy (confirm before
// archiving a running session, keep archived rows until navigating away, ask for the first profile
// before anything needs one, …).
import { getContext, setContext } from 'svelte';
import { visibleSessions } from '../model/filters';
import { GROUPS, groupSessions, type Group, type GroupBy } from '../model/grouping';
import { deepLinkHash } from '../model/misc';
import { scopeToProfile } from '../model/profiles';
import { UNASSIGNED, type Ctx, type Scope } from '../model/projects';
import { busy } from '../model/status';
import { taskPrompt } from '../model/tasks';
import { randomUuid } from '../model/uuid';
import { leadsToMatch, treeIndex, treeRoots, type TreeIndex } from '../model/tree';
import type { FeedSession, OpId, Profile, ProfileId, Project, SessionDetail, SessionId, Task, TaskStatus } from '../protocol';
import type { Client } from '../state/client.svelte';
import { Accounts } from './accounts.svelte';
import { Dialogs, Toasts } from './overlays.svelte';
import { Prefs } from './prefs.svelte';
import { InputHeartbeat, pageShown } from './presence';
import { View, type MovePreset, type NewSessionPreset, type SessionRef, type SettingsTab } from './view.svelte';

/** A fresh idempotency key for one user action. */
export const newOpId = (): OpId => randomUuid();

/** The common params of a pass-through command on a session. */
export const onSession = (s: SessionRef) => ({ op_id: newOpId(), id: s.id });

export class App {
  readonly prefs = new Prefs();
  readonly view = new View();
  readonly dialogs = new Dialogs();
  readonly toasts = new Toasts();
  /** The accounts of model providers with several. */
  readonly accounts: Accounts;
  /** User input, reported in the background (failures stay silent) so pushes wait while the user is here. */
  private readonly heartbeat = new InputHeartbeat(() => {
    this.client.request('client.activity', {}).catch(() => {});
  });

  /** Profiles in the user's order: none on a fresh install or from a server before profiles. */
  readonly profiles: Profile[];
  /** Several profiles: only then do the views name them (one profile looks like none). */
  readonly multi: boolean;
  /** The profile the views show (the only one, with one); null without profiles. */
  readonly profile: Profile | null;
  /** The profile Settings' Machines, Models and Sandbox show (the current one unless picked). */
  readonly settingsProfile: Profile | null;
  /** Everything, of every profile. */
  readonly all: Ctx;
  /** The current profile's data, which every scoped view reads (everything with one profile). */
  readonly ctx: Ctx;
  /** What notifications and the bell count: every profile under ∗, else the scope. */
  readonly notesCtx: Ctx;
  private readonly byProfile: ReadonlyMap<ProfileId, Ctx>;
  /** The rail's scope. */
  readonly scope: Scope;
  /** The scope's project, when it is one (Unassigned and everything aren't). */
  readonly project: Project | null;
  readonly current: FeedSession | undefined;
  /** The session list as shown: visible sessions, their groups and the subagent tree. */
  readonly listing: { visible: Set<string>; tree: TreeIndex; groups: Group[] };

  constructor(readonly client: Client) {
    client.onError = (message) => this.toasts.show(message, 'error');
    this.accounts = new Accounts(client);
    const feed = client.feed;
    this.profiles = $derived(feed.profileList);
    this.multi = $derived(this.profiles.length > 1);
    this.profile = $derived(this.profiles.find((p) => p.id === this.view.profile) ?? this.profiles[0] ?? null);
    this.settingsProfile = $derived(this.profiles.find((p) => p.id === this.view.settingsProfile) ?? this.profile);
    this.all = $derived({
      machines: feed.machines,
      machineList: feed.machineList,
      projects: feed.projectList,
      sessions: feed.sessionList,
      tasks: feed.taskList,
      overlaps: feed.overlaps,
    });
    this.byProfile = $derived(new Map(this.multi ? this.profiles.map((p) => [p.id, scopeToProfile(this.all, p.id)]) : []));
    this.ctx = $derived((this.profile && this.byProfile.get(this.profile.id)) || this.all);
    this.project = $derived((this.view.project && this.view.project !== UNASSIGNED && this.ctx.projects.find((p) => p.id === this.view.project)) || null);
    // With several profiles there is no Unassigned: a profile's ∗ groups the sessions on no task.
    this.scope = $derived(this.view.project === UNASSIGNED && !this.multi ? UNASSIGNED : this.project);
    this.notesCtx = $derived(this.scope ? this.ctx : this.all);
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

  /** A profile's data (everything with one profile or none). */
  ctxOf(id: ProfileId | null | undefined): Ctx {
    return (id && this.byProfile.get(id)) || this.all;
  }

  /** The profile to name next to an object (chips, hints): only with several profiles. */
  shownProfile(id: ProfileId | null | undefined): Profile | null {
    return (this.multi && this.profiles.find((p) => p.id === id)) || null;
  }

  // -------------------------------------------------------------------------------- navigation

  /** Opens a session; one of another profile (a deep link, the palette, a notification) enters it. */
  open(ref: SessionRef): void {
    const owner = this.client.feed.sessions.get(ref.id)?.profile_id;
    if (this.multi && owner && owner !== this.profile?.id) this.enterProfile(owner);
    if (!this.view.keep.includes(ref.id)) this.view.keep = [];
    this.view.current = { id: ref.id };
    this.view.page = 'sessions';
    this.view.phoneDetail = true;
    this.view.drawer = false;
    if (typeof history !== 'undefined') history.replaceState(null, '', deepLinkHash(ref.id));
  }

  /** Scopes the views to a project, to Unassigned, or to everything (null), in `profile` if given. */
  openProject(id: string | null, profile?: ProfileId): void {
    const entering = profile !== undefined && profile !== this.profile?.id;
    if (entering) this.enterProfile(profile);
    this.view.keep = [];
    this.view.project = id;
    this.view.openTask = null;
    this.view.drawer = false;
    if (this.view.settings && this.prefs.settingsPage) this.view.settings = null;
    if (entering) this.leaveOtherProfiles();
  }

  /** Enters another profile at its ∗. */
  private enterProfile(id: ProfileId): void {
    this.view.profile = id;
    this.view.project = null;
    this.view.openTask = null;
    this.view.keep = [];
  }

  /** The open session stays only if it is the current profile's; the desktop opens its most urgent instead. */
  private leaveOtherProfiles(): void {
    const cur = this.current;
    if (!cur || cur.profile_id === this.profile?.id) return;
    this.view.current = null;
    this.view.phoneDetail = false;
    const first = this.orderedSessions()[0];
    if (first && this.desk) this.open(first);
  }

  openTask(id: string): void {
    this.view.openTask = id;
    if (this.desk) {
      if (!this.prefs.topPin) this.view.topPeek = true;
    } else this.view.page = 'tasks';
  }

  openSettings(tab: SettingsTab = 'machines'): void {
    this.view.drawer = false;
    if (!this.view.settings) this.view.settingsProfile = null;
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

  /**
   * Runs an action that needs a profile with the current one. On a fresh install (no profile yet)
   * it first asks for one in the "New profile" dialog, then carries on in it. A server before
   * profiles needs none (`null`).
   */
  withProfile(run: (profile: ProfileId | null) => void): void {
    if (!this.client.profilesSupported) return run(null);
    if (this.profile) return run(this.profile.id);
    this.view.modal = {
      kind: 'profile',
      id: null,
      then: (p) => {
        this.view.profile = p.id;
        run(p.id);
      },
    };
  }

  /** The new-session dialog, in the preset's profile or the current one. */
  newSession(preset: NewSessionPreset = {}): void {
    this.withProfile((id) => (this.view.modal = { kind: 'new-session', preset: { ...preset, profile: preset.profile ?? id } }));
  }

  startTask(t: Task): void {
    this.newSession({ tasks: [t.id], prompt: taskPrompt(t), profile: t.profile_id });
  }

  /** The new-task dialog, pre-assigned to the project being viewed. */
  newTask(status: TaskStatus = 'todo', sessions: SessionId[] = []): void {
    this.withProfile((profile) => {
      this.view.modal = { kind: 'new-task', preset: { status, project_ids: this.project ? [this.project.id] : [], sessions, profile } };
    });
  }

  newProject(): void {
    this.view.drawer = false;
    this.withProfile((profile) => (this.view.modal = { kind: 'project', id: null, profile }));
  }

  /** The move dialog: machines, projects (with their tasks) or sessions to another profile. */
  moveToProfile(preset: MovePreset): void {
    this.view.modal = { kind: 'move', preset };
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

  /** Reports what this tab shows, for push deduplication and presence. The next input reports at
   * once (the tab may have been shown again, or the connection reopened). */
  reportFocus(): void {
    if (!this.client.online) return;
    const c = this.view.current;
    const showing = c && (this.desk || this.view.phoneDetail) && !this.view.settings;
    const visible = typeof document === 'undefined' || pageShown();
    this.heartbeat.reset();
    void this.client.call('client.focus', {
      device_id: this.client.device.id,
      session_id: showing ? c.id : null,
      visible,
    });
  }

  /** Reports user input: throttled, and only while the tab is shown. */
  reportInput(): void {
    if (this.client.online) this.heartbeat.input();
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
