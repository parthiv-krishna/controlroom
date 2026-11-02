// Resolving another session named by the daemon (`PeerRef`, by daemon id) to a session in the feed.
import type { App } from '../app/app.svelte';
import type { SessionRef } from '../app/view.svelte';
import { sessionKey } from '../model/projects';
import type { MachineSession, PeerRef } from '../protocol';

/** The peer's session if this controller knows its machine and the session; else null. */
export function peerSession(app: App, peer: PeerRef): (SessionRef & { session: MachineSession }) | null {
  for (const m of app.client.feed.machineList) {
    if (m.daemon_id !== peer.daemon_id) continue;
    const session = app.client.feed.sessions.get(sessionKey(m.id, peer.session_id));
    return session ? { machine_id: m.id, id: peer.session_id, session } : null;
  }
  return null;
}
