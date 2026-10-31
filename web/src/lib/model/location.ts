// Locations (`host:path`, with the user's home shown as `~`), project location prefixes, and the
// host-rooted directory tree behind Group by Location and the Location filter.
import type { LocationPrefix, Machine, MachineId, MachineSession, RemotePath } from '../protocol';

/** `/home/<user>/x` → `~/x` (paths arrive with `~` expanded). */
export function normPath(cwd: RemotePath, user: string): string {
  const home = `/home/${user}`;
  if (cwd === home) return '~';
  return cwd.startsWith(home + '/') ? '~' + cwd.slice(home.length) : cwd;
}

export const loc = (s: { cwd: RemotePath }, m: Machine): string => `${m.name}:${normPath(s.cwd, m.user)}`;

/** A filter value matches its subtree: `host:` is the whole machine, `host:path` that dir and below. */
export function underLoc(location: string, prefix: string): boolean {
  if (prefix.endsWith(':') || prefix.endsWith('/')) return location.startsWith(prefix);
  return location === prefix || location.startsWith(prefix + '/');
}

/** Whether a session is at a project location prefix (the directory and everything below it). */
export function prefixMatch(s: { cwd: RemotePath }, m: Machine, p: LocationPrefix): boolean {
  if (p.machine_id && p.machine_id !== m.id) return false;
  if (!p.path_prefix) return !!p.machine_id;
  const want = normPath(p.path_prefix.replace(/(.)\/+$/, '$1'), m.user);
  const have = normPath(s.cwd, m.user);
  return have === want || have.startsWith(want === '/' ? '/' : want + '/');
}

/** A prefix as text: `host:path`, `host:` (whole machine) or `~/path` (any machine). */
export function prefixText(p: LocationPrefix, machines: ReadonlyMap<MachineId, Machine>): string {
  if (!p.machine_id) return p.path_prefix;
  const m = machines.get(p.machine_id);
  return `${m?.name ?? '?'}:${m ? normPath(p.path_prefix, m.user) : p.path_prefix}`;
}

/** Parses `host:path`, `host:`, `~/path` or `/path`; null for an unknown host. */
export function parsePrefix(text: string, machines: Iterable<Machine>): LocationPrefix | null {
  const t = text.trim();
  const i = t.indexOf(':');
  if (i < 0) return { machine_id: null, path_prefix: t };
  const name = t.slice(0, i);
  for (const m of machines) if (m.name === name) return { machine_id: m.id, path_prefix: t.slice(i + 1) };
  return null;
}

// ------------------------------------------------------------------------------------------- tree

export interface LocNode {
  label: string;
  path: string;
  kids: Map<string, LocNode>;
  sessions: MachineSession[];
}
export interface HostTree {
  machine: Machine;
  root: LocNode;
}

export const sortedKids = (n: LocNode): LocNode[] => [...n.kids.values()].sort((a, b) => a.label.localeCompare(b.label));

/** Host-rooted trees; directories without sessions and with one child fold (`~/src`, not `~` › `src`). */
export function locTree(sessions: readonly MachineSession[], machines: readonly Machine[]): HostTree[] {
  const out: HostTree[] = [];
  for (const m of machines) {
    const here = sessions.filter((s) => s.machine_id === m.id);
    if (!here.length) continue;
    const root: LocNode = { label: '', path: '', kids: new Map(), sessions: [] };
    for (const s of here) {
      let node = root;
      const acc: string[] = [];
      for (const seg of normPath(s.cwd, m.user).split('/')) {
        acc.push(seg);
        let kid = node.kids.get(seg);
        if (!kid) {
          kid = { label: seg === '' ? '/' : seg, path: acc.join('/') || '/', kids: new Map(), sessions: [] };
          node.kids.set(seg, kid);
        }
        node = kid;
      }
      node.sessions.push(s);
    }
    fold(root);
    out.push({ machine: m, root });
  }
  return out;
}

function fold(n: LocNode): void {
  for (const k of n.kids.values()) {
    while (!k.sessions.length && k.kids.size === 1) {
      const [only] = k.kids.values();
      k.label = (k.label === '/' ? '' : k.label) + '/' + only.label;
      k.path = only.path;
      k.kids = only.kids;
      k.sessions = only.sessions;
    }
    fold(k);
  }
}

export interface LocOption {
  value: string;
  depth: number;
  label: string;
  machine: Machine;
  host: boolean;
}

/** The tree flattened for the Location filter. */
export function locOptions(trees: HostTree[]): LocOption[] {
  const out: LocOption[] = [];
  for (const { machine, root } of trees) {
    out.push({ value: machine.name + ':', depth: 0, label: machine.name, machine, host: true });
    const walk = (n: LocNode, depth: number) => {
      for (const k of sortedKids(n)) {
        out.push({ value: `${machine.name}:${k.path}`, depth, label: k.label, machine, host: false });
        walk(k, depth + 1);
      }
    };
    walk(root, 1);
  }
  return out;
}
