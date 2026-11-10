// Sandbox levels in the new-session dialog and a session's header (spec §11.6). The session's own
// `SandboxInfo` comes from the server; before a session exists, the dialog previews its default
// level from the profiles as the server computes it: the strictest level among its workspaces'
// machines and its tasks' projects.
import type { MachineId, ProjectId, SandboxInfo, SandboxLevel, SandboxProfile } from '../protocol';

export const LEVELS: { value: SandboxLevel; label: string; hint: string }[] = [
  { value: 'strict', label: 'Strict', hint: 'Workspace directories and session dirs only' },
  { value: 'relaxed', label: 'Relaxed', hint: 'Plus read-only home config (git, …)' },
  { value: 'off', label: 'Off', hint: 'Unconfined: the agent can read and write anything your user can' },
];
export const levelLabel = (l: SandboxLevel): string => LEVELS.find((x) => x.value === l)?.label ?? l;

const STRICTNESS: Record<SandboxLevel, number> = { strict: 2, relaxed: 1, off: 0, unknown: 2 };

/** Whether `level` confines less than `than`. */
export const looser = (level: SandboxLevel, than: SandboxLevel): boolean => STRICTNESS[level] < STRICTNESS[than];

/** The red warning of level off, wherever it is chosen or shown. */
export const OFF_WARNING = 'Not sandboxed: the agent can read and write anything your user can.';

/** The confirmation of a session's level change. */
export interface LevelConfirm {
  title: string;
  body: string;
  ok: string;
  danger: boolean;
}

/**
 * What setting a session's level to `next` (`null`: back to the defaults) asks first: `null` when the
 * level it runs at stays the same (only `explicit` changes; nothing restarts), else a confirmation of
 * the restart. Off gets the red treatment.
 */
export function levelChange(info: SandboxInfo, next: SandboxLevel | null): LevelConfirm | null {
  const level = next ?? info.default_level;
  if (level === info.level) return null;
  const body = 'A running turn stops; the agent’s shells restart. Running jobs and your terminals keep the old sandbox until they end.';
  if (level === 'off') return { title: 'Restart the agent without a sandbox?', body: `${OFF_WARNING} ${body}`, ok: 'Turn off and restart', danger: true };
  const name = levelLabel(level).toLowerCase();
  return { title: next === null ? `Restart the agent with the defaults (${name})?` : `Restart the agent with ${name}?`, body, ok: 'Restart', danger: false };
}

/** The default level of a new session on `machines` linked to tasks of `projects` (`strict` when nothing is set). */
export function defaultLevel(profiles: readonly SandboxProfile[], machines: readonly MachineId[], projects: readonly ProjectId[]): SandboxLevel {
  const levels = profiles
    .filter((p) => (p.scope.kind === 'machine' && machines.includes(p.scope.machine_id)) || (p.scope.kind === 'project' && projects.includes(p.scope.project_id)))
    .map((p) => p.level)
    .filter((l): l is SandboxLevel => l !== null);
  if (!levels.length) return 'strict';
  return levels.reduce((a, b) => (STRICTNESS[b] > STRICTNESS[a] ? b : a));
}

/** A path typed for a standing grant, a socket or the never list, checked as the server checks it. */
export type ProfilePath = { ok: true; path: string } | { ok: false; error: string };

/**
 * Checks and normalizes a grant, socket or never path: absolute (`/…`) or home-relative (`~`, `~/…`;
 * each host expands `~`), no `.` or `..` components; repeated and trailing slashes are dropped.
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
  return { ok: true, path };
}

/**
 * Directories under a home that contain a path nono 0.80 always denies (`~/.ssh`, `~/.config/gcloud`,
 * …), with one such path: nono refuses to start with any of them granted. Mirrors
 * `controlroom_sandbox::limits` (the server's check, whose refusal the tab shows as well).
 */
const CONTAINS_DENIED: Record<string, string> = {
  '': '.ssh',
  '.config': '.config/gcloud',
  '.config/bun': '.config/bun/bunfig.toml',
  '.local': '.local/share/keyrings',
  '.local/share': '.local/share/keyrings',
  '.mozilla': '.mozilla/firefox',
};

/**
 * Why a standing path grant of `path` (normalized) could never take effect, if it couldn't: another
 * process's `/proc/<pid>` entry, nono's own state (`~/.nono`), or a directory containing a path
 * nono always denies (so `/`, `/home`, `~`, `~/.config`). `~`, `/home/<user>` and `/root` count as
 * homes.
 */
export function grantLimit(path: string): string | null {
  if (/^\/proc\/\d+(\/|$)/.test(path)) return 'it is one process’s /proc entry: it changes with every process, and nono never opens another process’s entries';
  if (path === '/' || path === '/home') return 'it contains every home, where nono always protects ~/.ssh and more';
  const home = /^(~|\/home\/[^/]+|\/root)(?:\/(.*))?$/.exec(path);
  if (!home) return null;
  const [, base, rest = ''] = home;
  if (rest === '.nono' || rest.startsWith('.nono/')) return 'it overlaps nono’s own state (~/.nono)';
  const denied = CONTAINS_DENIED[rest];
  return denied ? `it contains ${base}/${denied}, which nono always protects, so nono would refuse to start` : null;
}

/** Sockets offered as one-click grants in Settings › Sandbox. None is granted by default. */
export const KNOWN_SOCKETS: { path: string; label: string; hint: string }[] = [
  {
    path: '/run/dbus/system_bus_socket',
    label: 'D-Bus system bus',
    hint: 'systemctl, journalctl, busctl, loginctl…: read-only queries work without authentication; privileged actions still need polkit. systemd 254 and later bind their clients to an abstract socket name, which the sandbox refuses, so their tools still can’t connect; dbus-send and gdbus can.',
  },
  { path: '/run/systemd/journal/stdout', label: 'systemd journal (stdout)', hint: 'systemd-cat: writes to the journal' },
  { path: '/dev/log', label: 'syslog', hint: 'logger: writes to the system log' },
];

/** A one-click path grant offered in Settings › Sandbox. */
export const PROC_GRANT = {
  path: '/proc',
  label: 'Process listing (/proc, read-only)',
  hint: 'systemctl and ps stop asking about /proc. nono still never opens another process’s /proc entries, so ps, top and pgrep list only themselves.',
};
