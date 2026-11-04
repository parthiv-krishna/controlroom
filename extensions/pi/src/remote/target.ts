// `machine:path` addressing (design §5). Every path the agent writes names a machine of one of
// the session's workspaces; `~` is that machine's home and a relative path resolves against
// its workspace root (the agent daemon resolves both). A bare path is accepted only when the
// session has exactly one workspace, so the agent can never act on the wrong machine.

import type { RemotePath } from '../protocol/RemotePath.ts';
import type { SessionInfo } from '../protocol/SessionInfo.ts';
import type { WorkspaceInfo } from '../protocol/WorkspaceInfo.ts';

/** Where a tool argument points. */
export interface Target {
  /** The machine's display name. */
  machine: string;
  /** The path on it as written (relative, `~` or absolute); `null` for its workspace root. */
  path: RemotePath | null;
  /** The argument as `machine:path`, for messages. */
  display: string;
}

/** A machine prefix: a display name has no `:` and no `/`. */
const PREFIXED = /^([^:/\s]+):(.*)$/s;

/** Resolves a tool's path argument (omitted: the workspace root, with one workspace). */
export function parseTarget(info: SessionInfo, spec: string | null | undefined, what = 'path'): Target {
  const workspaces = info.workspaces;
  if (!workspaces.length) throw new Error('This session has no workspaces yet, so there is no machine to use.');
  const given = spec?.trim() ?? '';
  const prefixed = PREFIXED.exec(given);
  if (prefixed) {
    const [, machine, rest] = prefixed;
    if (!workspaces.some((w) => w.machine === machine)) {
      throw new Error(`Unknown machine "${machine}" in ${what} "${given}". ${listWorkspaces(info)}`);
    }
    const path = rest.trim() || null;
    return { machine, path, display: path ? `${machine}:${path}` : workspaceDisplay(workspaces.find((w) => w.machine === machine)!) };
  }
  if (workspaces.length > 1) {
    const example = `${workspaces[0].machine}:${given || workspaces[0].path}`;
    const hint = given ? `"${given}" names no machine` : `No ${what} given`;
    throw new Error(`${hint}, and this session has several workspaces: write machine:path, e.g. ${example}. ${listWorkspaces(info)}`);
  }
  const only = workspaces[0];
  return given ? { machine: only.machine, path: given, display: `${only.machine}:${given}` } : { machine: only.machine, path: null, display: workspaceDisplay(only) };
}

/** A workspace as `machine:path` (the directory as given). */
export function workspaceDisplay(w: WorkspaceInfo): string {
  return `${w.machine}:${w.path}`;
}

/** An absolute host path as `machine:~/…` under that machine's home, else `machine:/…`. */
export function shown(info: SessionInfo, machine: string, path: RemotePath): string {
  const home = info.workspaces.find((w) => w.machine === machine && w.home)?.home;
  if (home && home !== '/' && (path === home || path.startsWith(`${home}/`))) return `${machine}:~${path.slice(home.length)}`;
  return `${machine}:${path}`;
}

/** "Workspaces: devbox:~/src/api, buildbox:~/work/web." */
export function listWorkspaces(info: SessionInfo): string {
  return `Workspaces: ${info.workspaces.map(workspaceDisplay).join(', ')}.`;
}
