// Client state from the `state.subscribe` feed (spec §5.4): a snapshot, then `state.events`
// changes in rev order within an epoch. Each object kind lives in its own SvelteMap, and a change
// replaces exactly one entry, so only the components reading that entry (or iterating the map)
// update. The UI derives nothing: every object is shown as the server sent it.
import { SvelteMap } from 'svelte/reactivity';
import type {
  ContactGrant,
  Device,
  FeedSession,
  Machine,
  Overlap,
  Project,
  Schedule,
  StateChange,
  StateCursor,
  StateSnapshot,
  Task,
} from '../protocol';

export type ApplyResult = 'applied' | 'gap';

export class StateFeed {
  readonly machines = new SvelteMap<string, Machine>();
  /** Global sessions by id. */
  readonly sessions = new SvelteMap<string, FeedSession>();
  readonly projects = new SvelteMap<string, Project>();
  readonly tasks = new SvelteMap<string, Task>();
  readonly grants = new SvelteMap<string, ContactGrant>();
  readonly schedules = new SvelteMap<string, Schedule>();
  readonly devices = new SvelteMap<string, Device>();
  /** Directories several open sessions share (replaced whole). */
  overlaps = $state.raw<Overlap[]>([]);
  /** Set by the first snapshot. */
  loaded = $state(false);
  /** The last applied position (not reactive: only the sync engine reads it). */
  cursor: StateCursor | null = null;

  readonly machineList = $derived([...this.machines.values()].sort((a, b) => a.name.localeCompare(b.name)));
  readonly sessionList = $derived([...this.sessions.values()]);
  readonly projectList = $derived([...this.projects.values()]);
  readonly taskList = $derived([...this.tasks.values()]);
  readonly grantList = $derived([...this.grants.values()]);
  readonly scheduleList = $derived([...this.schedules.values()]);
  readonly deviceList = $derived([...this.devices.values()]);

  /** Replaces everything with a snapshot at `cursor`. */
  reset(snapshot: StateSnapshot, cursor: StateCursor): void {
    fill(this.machines, snapshot.machines, (m) => m.id);
    fill(this.sessions, snapshot.sessions, (s) => s.id);
    this.overlaps = snapshot.overlaps;
    fill(this.projects, snapshot.projects, (p) => p.id);
    fill(this.tasks, snapshot.tasks, (t) => t.id);
    fill(this.grants, snapshot.grants, (g) => g.id);
    fill(this.schedules, snapshot.schedules, (s) => s.id);
    fill(this.devices, snapshot.devices, (d) => d.device_id);
    this.cursor = cursor;
    this.loaded = true;
  }

  /** Continues from a resubscribe without a snapshot: the cursor moves, events follow. */
  resume(cursor: StateCursor): void {
    this.cursor = cursor;
  }

  /**
   * Applies changes in rev order. Revisions at or before the cursor are skipped (overlap after a
   * resubscribe). A skipped revision is a gap: nothing more is applied and the caller resubscribes.
   */
  apply(changes: readonly StateChange[]): ApplyResult {
    if (!this.cursor) return 'gap';
    for (const change of changes) {
      if (change.rev <= this.cursor.rev) continue;
      if (change.rev !== this.cursor.rev + 1) return 'gap';
      this.applyOne(change);
      this.cursor = { epoch: this.cursor.epoch, rev: change.rev };
    }
    return 'applied';
  }

  private applyOne(c: StateChange): void {
    switch (c.kind) {
      case 'machine_upserted': this.machines.set(c.machine.id, c.machine); break;
      case 'machine_removed': this.machines.delete(c.machine_id); break;
      case 'session_upserted': this.sessions.set(c.session.id, c.session); break;
      case 'session_removed': this.sessions.delete(c.session_id); break;
      case 'overlaps_changed': this.overlaps = c.overlaps; break;
      case 'project_upserted': this.projects.set(c.project.id, c.project); break;
      case 'project_removed': this.projects.delete(c.project_id); break;
      case 'task_upserted': this.tasks.set(c.task.id, c.task); break;
      case 'task_removed': this.tasks.delete(c.task_id); break;
      case 'grant_upserted': this.grants.set(c.grant.id, c.grant); break;
      case 'grant_removed': this.grants.delete(c.grant_id); break;
      case 'schedule_upserted': this.schedules.set(c.schedule.id, c.schedule); break;
      case 'schedule_removed': this.schedules.delete(c.schedule_id); break;
      case 'device_upserted': this.devices.set(c.device.device_id, c.device); break;
      case 'device_removed': this.devices.delete(c.device_id); break;
      default: // a newer server's change kind: nothing to show
    }
  }
}

function fill<T>(map: SvelteMap<string, T>, items: readonly T[], key: (item: T) => string): void {
  const keep = new Set(items.map(key));
  for (const k of [...map.keys()]) if (!keep.has(k)) map.delete(k);
  for (const item of items) map.set(key(item), item);
}
