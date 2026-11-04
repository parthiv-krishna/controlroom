// The subagent tree in the session list: children nest under their parent. When a filter
// matches only a subagent, its topmost ancestor is shown as a dimmed row so the tree stays
// intact, and under a dimmed ancestor only the branches leading to matches are shown.
import type { FeedSession } from '../protocol';

export interface TreeIndex {
  children(s: FeedSession): FeedSession[];
  parent(s: FeedSession): FeedSession | undefined;
}

/** Indexes parent links over all sessions. Archived children show only when `keep` holds them. */
export function treeIndex(all: readonly FeedSession[], keep: ReadonlySet<string>): TreeIndex {
  const byId = new Map(all.map((s) => [s.id, s]));
  const kids = new Map<string, FeedSession[]>();
  for (const s of all) {
    if (!s.parent_id || (s.state === 'closed' && !keep.has(s.id))) continue;
    const list = kids.get(s.parent_id);
    if (list) list.push(s);
    else kids.set(s.parent_id, [s]);
  }
  return {
    children: (s) => kids.get(s.id) ?? [],
    parent: (s) => (s.parent_id ? byId.get(s.parent_id) : undefined),
  };
}

export interface TreeRow {
  s: FeedSession;
  /** Shown only because a descendant matches. */
  dim: boolean;
}

/** The roots to render for a group's matching sessions. */
export function treeRoots(items: readonly FeedSession[], visible: ReadonlySet<string>, idx: TreeIndex): TreeRow[] {
  const seen = new Set<string>();
  const out: TreeRow[] = [];
  for (const s of items) {
    let root = s;
    for (let p = idx.parent(root); p; p = idx.parent(root)) root = p;
    // a visible root renders the subtree (possibly in another group)
    if ((root !== s && visible.has(root.id)) || seen.has(root.id)) continue;
    seen.add(root.id);
    out.push({ s: root, dim: root !== s });
  }
  return out;
}

/** Whether a session or one of its descendants matches. */
export function leadsToMatch(s: FeedSession, visible: ReadonlySet<string>, idx: TreeIndex): boolean {
  return visible.has(s.id) || idx.children(s).some((c) => leadsToMatch(c, visible, idx));
}
