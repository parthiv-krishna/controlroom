// Where the extension finds its session: the launch env file of a controlroom session sets
// these variables. Without them pi wasn't started by controlroom and the extension stays off.

/** How to reach the session's controlroom daemon. */
export interface ControlroomEnv {
  /** The controlroom session id. */
  sessionId: string;
  /** The session's agent socket. */
  socket: string;
  /** The launch's token, sent in `hello`. */
  token: string;
}

/** Reads the controlroom variables; `null` unless all of them are set. */
export function readEnv(env: Record<string, string | undefined>): ControlroomEnv | null {
  const sessionId = env.CONTROLROOM_SESSION_ID?.trim();
  const socket = env.CONTROLROOM_AGENT_SOCKET?.trim();
  const token = env.CONTROLROOM_AGENT_TOKEN?.trim();
  if (!sessionId || !socket || !token) return null;
  return { sessionId, socket, token };
}
