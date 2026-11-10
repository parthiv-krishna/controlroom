// The user's own terminals in the dock (spec §22): shells the user opened from the "+" menu,
// private to them (the agent never sees them). They are sandboxed like the agent's shells, or
// unsandboxed: a login shell as the machine's SSH user, marked red everywhere.
import type { Shell } from '../protocol';

/** The name prefix the server gives the user's terminals. */
const PREFIX = 'you@';

/** One of the user's own terminals (an unknown owner counts as theirs). */
export const isMine = (s: Pick<Shell, 'owner'>): boolean => s.owner !== 'agent';

/** Runs outside the sandbox (an unknown mode counts as unsandboxed). */
export const isUnsandboxed = (s: Pick<Shell, 'owner' | 'sandbox'>): boolean => isMine(s) && s.sandbox !== 'workspace';

/** The tab's label: the user's terminals without their `you@` prefix (the tab says "you"). */
export const tabLabel = (s: Pick<Shell, 'name' | 'owner'>): string => (isMine(s) && s.name.startsWith(PREFIX) ? s.name.slice(PREFIX.length) : s.name);

/** The one-line confirm of an unsandboxed terminal. */
export const unsandboxedConfirm = (user: string | null, machine: string): string =>
  `Runs as ${user ?? 'your SSH user'} on ${machine} without the sandbox.`;
