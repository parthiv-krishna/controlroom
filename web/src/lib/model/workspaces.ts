// A session's workspaces (`machine:path`, one or more, on any machines), their machines'
// availability, and the directories several sessions share (overlaps). Sessions never go stale
// with a machine; only their workspaces become unavailable.
import type { Machine, MachineId, Overlap, RemotePath, SessionId, Workspace } from '../protocol';
import { ago } from './status';

/** The fields of a session the workspace helpers read. */
export interface HasWorkspaces {
  id: SessionId;
  workspaces: readonly Workspace[];
}

/** `/home/<user>/x` → `~/x` (resolved roots arrive with `~` expanded). */
export function normPath(path: RemotePath, user: string): string {
  const home = `/home/${user}`;
  if (path === home) return '~';
  return path.startsWith(home + '/') ? '~' + path.slice(home.length) : path;
}

/** A remote file as the agent names it: `machine:path`. */
export const machinePath = (machine: string, path: string): string => `${machine}:${path}`;

/** A directory on a machine as `machine:path`, with that user's home as `~`. */
export function dirLabel(machineId: MachineId, dir: RemotePath, machines: ReadonlyMap<MachineId, Machine>): string {
  const m = machines.get(machineId);
  return machinePath(m?.name ?? '?', m ? normPath(dir, m.user) : dir);
}

/** A workspace's directory with home as `~` (the resolved root once the host opened it). */
export function wsDir(w: Workspace, machines: ReadonlyMap<MachineId, Machine>): string {
  const user = machines.get(w.machine_id)?.user;
  const dir = w.root ?? w.path;
  return user ? normPath(dir, user) : dir;
}

/** A workspace as `machine:path`. */
export const wsLabel = (w: Workspace, machines: ReadonlyMap<MachineId, Machine>): string => dirLabel(w.machine_id, w.root ?? w.path, machines);

/** A path on a job's machine as `machine:path` (as the agent reads the log); bare when the machine is unknown. */
export const onMachine = (machineId: MachineId | null, path: RemotePath, machines: ReadonlyMap<MachineId, Machine>): string =>
  machineId ? dirLabel(machineId, path, machines) : path;

/** The primary workspace (relative paths and new shells default to it). */
export const primaryWorkspace = (s: HasWorkspaces): Workspace | undefined => s.workspaces.find((w) => w.primary) ?? s.workspaces[0];

/** Whether a session has a workspace on the machine. */
export const touches = (s: HasWorkspaces, machineId: MachineId): boolean => s.workspaces.some((w) => w.machine_id === machineId);

/** The session's machines, primary first, each once. */
export const machinesOf = (s: HasWorkspaces): MachineId[] => [...new Set(s.workspaces.map((w) => w.machine_id))];

export const isAvailable = (w: Workspace): boolean => w.availability.connectivity.state === 'connected';

/** The workspaces whose machine can't be reached now. */
export const unavailable = (s: HasWorkspaces): Workspace[] => s.workspaces.filter((w) => !isAvailable(w));

/** "devbox connected", "devbox unreachable since 2h ago", … */
export function availabilityText(w: Workspace, machines: ReadonlyMap<MachineId, Machine>, now = Date.now()): string {
  const name = machines.get(w.machine_id)?.name ?? 'a removed machine';
  const state = w.availability.connectivity.state;
  if (state === 'connected') return `${name} connected`;
  const what = state === 'action_required' ? 'needs attention' : state === 'incompatible' ? 'outdated' : state;
  return `${name} ${what} since ${ago(w.availability.since, now)}`;
}

/** The overlaps a session is part of. */
export const overlapsOf = (s: HasWorkspaces, overlaps: readonly Overlap[]): Overlap[] => overlaps.filter((o) => o.session_ids.includes(s.id));

/** One run of text: plain, or a `machine:path` naming a known machine. */
export interface PathRun {
  text: string;
  machine?: string;
  path?: string;
}

/**
 * Splits text into plain runs and `machine:path` runs for the given machine names, so a path the
 * agent names can be shown (and later opened) as a remote file.
 */
export function splitMachinePaths(text: string, names: readonly string[]): PathRun[] {
  if (!names.length) return [{ text }];
  const escaped = names.map((n) => n.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).sort((a, b) => b.length - a.length);
  const re = new RegExp(`(?<![\\w.-])(${escaped.join('|')}):([^\\s"'\`,;)\\]}]+)`, 'g');
  const out: PathRun[] = [];
  let at = 0;
  for (const m of text.matchAll(re)) {
    const i = m.index ?? 0;
    if (i > at) out.push({ text: text.slice(at, i) });
    out.push({ text: m[0], machine: m[1], path: m[2] });
    at = i + m[0].length;
  }
  if (at < text.length) out.push({ text: text.slice(at) });
  return out;
}
