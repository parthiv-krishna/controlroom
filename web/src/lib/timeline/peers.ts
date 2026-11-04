// Resolving another session a timeline names (`PeerRef`) to a session in the feed. Sessions are
// global, so the session id alone identifies it.
import type { App } from '../app/app.svelte';
import type { FeedSession, PeerRef } from '../protocol';

/** The peer's session, if the feed has it; else null. */
export const peerSession = (app: App, peer: PeerRef): FeedSession | null => app.client.feed.sessions.get(peer.session_id) ?? null;
