// View state for this visit: selection, list filters, peeking panes, modals, drafts.
import { emptyFilters, type FacetKey, type Filters } from '../model/filters';
import type { MachineId, ProjectId, SessionId, TaskId, TaskStatus } from '../protocol';

export interface SessionRef {
  machine_id: MachineId;
  id: SessionId;
}

export interface NewSessionPreset {
  machine_id?: MachineId;
  cwd?: string;
  tasks?: TaskId[];
  prompt?: string;
}
export interface NewTaskPreset {
  status?: TaskStatus;
  project_ids?: ProjectId[];
  /** `<machine id>/<session id>` values. */
  sessions?: string[];
}

export type Modal =
  | { kind: 'new-session'; preset: NewSessionPreset }
  | { kind: 'new-task'; preset: NewTaskPreset }
  | { kind: 'project'; id: ProjectId | null; paths?: string[] }
  | { kind: 'start-fresh'; session: SessionRef }
  | { kind: 'link-tasks'; session: SessionRef }
  | { kind: 'task'; id: TaskId };

/** Where the raw view was opened: an item's records, or the live tail (all null). */
export interface RawFocus {
  from: number | null;
  to: number | null;
  item: string | null;
}

export type SettingsTab = 'machines' | 'keys' | 'notifications' | 'devices' | 'sandbox' | 'contacts' | 'schedules' | 'keyboard' | 'appearance';

export class View {
  project = $state<ProjectId | null>(null);
  current = $state<SessionRef | null>(null);
  openTask = $state<TaskId | null>(null);
  /** The session list. */
  q = $state('');
  filters = $state<Filters>(emptyFilters());
  collapsed = $state<Record<string, boolean>>({});
  /** Sessions archived during this visit stay listed (↺ undo) until the user navigates away. */
  keep = $state<string[]>([]);
  menu = $state<'filter' | 'group' | null>(null);
  filterSection = $state<FacetKey>('state');
  boardQ = $state('');
  /** Unpinned panes peeking over the content. */
  leftPeek = $state(false);
  topPeek = $state(false);
  notesOpen = $state(false);
  /** Phone. */
  page = $state<'sessions' | 'tasks'>('sessions');
  phoneDetail = $state(false);
  drawer = $state(false);
  wide = $state(typeof window === 'undefined' || window.innerWidth > 760);
  /** Overlays. */
  settings = $state<SettingsTab | null>(null);
  modal = $state<Modal | null>(null);
  palette = $state<string | null>(null);
  help = $state(false);
  /** Composer drafts by session key. */
  drafts = $state<Record<string, string>>({});
  /** The last list search, for n/N. */
  lastSearch = $state('');
  /** Session panels folded or opened by the user, by `<panel>:<session key>`. */
  panels = $state<Record<string, boolean>>({});
  /** Timeline items expanded or collapsed by the user, by `<session key>#<item id>`. */
  expanded = $state<Record<string, boolean>>({});
  /** The open raw view per session key, and the records it was opened at. */
  raw = $state<Record<string, RawFocus>>({});
  /** A timeline item to scroll to and flash (a raw record's link back). */
  focusItem = $state<{ key: string; id: string } | null>(null);

  get filtering(): boolean {
    return !!this.q.trim() || Object.values(this.filters).some((x) => x.length);
  }

  clearFilters(): void {
    this.filters = emptyFilters();
    this.q = '';
    this.keep = [];
  }
}
