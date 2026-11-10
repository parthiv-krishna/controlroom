// Sandbox levels in the new-session dialog (spec §11.6). The session's own `SandboxInfo` comes from
// the server; before a session exists, the dialog previews its default level from the profiles as
// the server computes it: the strictest level among its workspaces' machines and its tasks'
// projects.
import type { MachineId, ProjectId, SandboxLevel, SandboxProfile } from '../protocol';

export const LEVELS: { value: SandboxLevel; label: string; hint: string }[] = [
  { value: 'strict', label: 'Strict', hint: 'Workspace directories and session dirs only' },
  { value: 'relaxed', label: 'Relaxed', hint: 'Plus read-only home config (git, …)' },
  { value: 'off', label: 'Off', hint: 'Unconfined: the agent can read and write anything your user can' },
];
export const levelLabel = (l: SandboxLevel): string => LEVELS.find((x) => x.value === l)?.label ?? l;

const STRICTNESS: Record<SandboxLevel, number> = { strict: 2, relaxed: 1, off: 0, unknown: 2 };

/** Whether `level` confines less than `than`. */
export const looser = (level: SandboxLevel, than: SandboxLevel): boolean => STRICTNESS[level] < STRICTNESS[than];

/** The default level of a new session on `machines` linked to tasks of `projects` (`strict` when nothing is set). */
export function defaultLevel(profiles: readonly SandboxProfile[], machines: readonly MachineId[], projects: readonly ProjectId[]): SandboxLevel {
  const levels = profiles
    .filter((p) => (p.scope.kind === 'machine' && machines.includes(p.scope.machine_id)) || (p.scope.kind === 'project' && projects.includes(p.scope.project_id)))
    .map((p) => p.level)
    .filter((l): l is SandboxLevel => l !== null);
  if (!levels.length) return 'strict';
  return levels.reduce((a, b) => (STRICTNESS[b] > STRICTNESS[a] ? b : a));
}

/** A path typed for a standing grant or the never list, checked as the server checks it. */
export type ProfilePath = { ok: true; path: string; broad: boolean } | { ok: false; error: string };

/** Paths so broad that granting them deserves a confirmation. */
const BROAD = new Set(['/', '~', '/home']);

/**
 * Checks and normalizes a grant or never path: absolute (`/…`) or home-relative (`~`, `~/…`; each
 * host expands `~`), no `.` or `..` components; repeated and trailing slashes are dropped. `broad`
 * marks `/`, `~` and `/home`.
 */
export function checkProfilePath(value: string): ProfilePath {
  const v = value.trim();
  let prefix: string;
  let rest: string;
  if (v === '~') [prefix, rest] = ['~', ''];
  else if (v.startsWith('~/')) [prefix, rest] = ['~', v.slice(2)];
  else if (v.startsWith('/')) [prefix, rest] = ['', v.slice(1)];
  else return { ok: false, error: 'Use an absolute path (/…) or one under your home (~/…)' };
  if (/[\u0000-\u001f\u007f]/.test(v)) return { ok: false, error: 'A path can’t contain control characters' };
  const parts = rest.split('/').filter((p) => p !== '');
  if (parts.some((p) => p === '.' || p === '..')) return { ok: false, error: 'Remove the . and .. parts' };
  const path = prefix + parts.map((p) => `/${p}`).join('') || '/';
  return { ok: true, path, broad: BROAD.has(path) };
}
