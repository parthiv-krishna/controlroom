// Sandbox levels in the new-session dialog (spec §11.6). The session's own `SandboxInfo` comes from
// the server; before a session exists, the dialog previews its default level from the profiles:
// the strictest level among the machine and every project whose location matches.
import type { Machine, Project, SandboxLevel, SandboxProfile } from '../protocol';
import { prefixMatch } from './location';

export const LEVELS: { value: SandboxLevel; label: string; hint: string }[] = [
  { value: 'strict', label: 'Strict', hint: 'Working directory and session dirs only' },
  { value: 'relaxed', label: 'Relaxed', hint: 'Plus read-only home config (git, …)' },
  { value: 'off', label: 'Off', hint: 'Unconfined: the agent can read and write anything your user can' },
];
export const levelLabel = (l: SandboxLevel): string => LEVELS.find((x) => x.value === l)?.label ?? l;

const STRICTNESS: Record<SandboxLevel, number> = { strict: 2, relaxed: 1, off: 0, unknown: 2 };

/** Whether `level` confines less than `than`. */
export const looser = (level: SandboxLevel, than: SandboxLevel): boolean => STRICTNESS[level] < STRICTNESS[than];

/** The default level a new session at `cwd` on `machine` would get (`strict` when nothing is set). */
export function defaultLevel(profiles: readonly SandboxProfile[], machine: Machine, cwd: string, projects: readonly Project[]): SandboxLevel {
  const matching = new Set(projects.filter((p) => p.location_prefixes.some((x) => prefixMatch({ cwd }, machine, x))).map((p) => p.id));
  const levels = profiles
    .filter((p) => (p.scope.kind === 'machine' && p.scope.machine_id === machine.id) || (p.scope.kind === 'project' && matching.has(p.scope.project_id)))
    .map((p) => p.level)
    .filter((l): l is SandboxLevel => l !== null);
  if (!levels.length) return 'strict';
  return levels.reduce((a, b) => (STRICTNESS[b] > STRICTNESS[a] ? b : a));
}
