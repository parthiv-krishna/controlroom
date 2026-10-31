// The subagent tree in the session list: children nest under their parent (same machine). When a
// filter matches only a subagent, its topmost ancestor is shown as a dimmed row so the tree stays
// intact, and under a dimmed ancestor only the branches leading to matches are shown.
import type { MachineSession } from '../protocol';
import { keyOf } from './projects';

export interface TreeIndex {
  children(s: MachineSession): MachineSession[];
  parent(s: MachineSession): MachineSession | undefined;
}

/** Indexes parent links over all sessions. Archived children show only when `keep` holds them. */
export function treeIndex(all: readonly MachineSession[], keep: ReadonlySet<string>): TreeIndex {
  const byKey = new Map(all.map((s) => [keyOf(s), s]));
  const kids = new Map<string, MachineSession[]>();
  for (const s of all) {
    if (!s.parent_id || (s.state === 'closed' && !keep.has(keyOf(s)))) continue;
    const pk = `${s.machine_id}/${s.parent_id}`;
    const list = kids.get(pk);
    if (list) list.push(s);
    else kids.set(pk, [s]);
  }
  return {
    children: (s) => kids.get(keyOf(s)) ?? [],
    parent: (s) => (s.parent_id ? byKey.get(`${s.machine_id}/${s.parent_id}`) : undefined),
  };
}

export interface TreeRow {
  s: MachineSession;
  /** Shown only because a descendant matches. */
  dim: boolean;
}

/** The roots to render for a group's matching sessions. */
export function treeRoots(items: readonly MachineSession[], visible: ReadonlySet<string>, idx: TreeIndex): TreeRow[] {
  const seen = new Set<string>();
  const out: TreeRow[] = [];
  for (const s of items) {
    let root = s;
    for (let p = idx.parent(root); p; p = idx.parent(root)) root = p;
    const k = keyOf(root);
    // a visible root renders the subtree (possibly in another group)
    if ((root !== s && visible.has(k)) || seen.has(k)) continue;
    seen.add(k);
    out.push({ s: root, dim: root !== s });
  }
  return out;
}

/** Whether a session or one of its descendants matches. */
export function leadsToMatch(s: MachineSession, visible: ReadonlySet<string>, idx: TreeIndex): boolean {
  return visible.has(keyOf(s)) || idx.children(s).some((c) => leadsToMatch(c, visible, idx));
}
