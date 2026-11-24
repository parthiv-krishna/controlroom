// Profiles are tenants (docs/design/profiles-tenants.md §10): every machine entry, project, task
// and session belongs to one (a machine entry may be shared by several). The views show one
// profile at a time: `scopeToProfile` derives its data from everything, once, for all of them.
// With one profile or none nothing is scoped and nothing about profiles is shown.
import type { FeedSession, Machine, ModelPolicy, Profile, ProfileId } from '../protocol';
import type { Ctx } from './projects';

/** Profiles in the user's order. */
export const sortProfiles = (list: readonly Profile[]): Profile[] =>
  [...list].sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));

/** Whether a policy admits an account (a pi provider id); `unknown` admits nothing. */
export function admits(policy: ModelPolicy, account: string): boolean {
  switch (policy.kind) {
    case 'any': return true;
    case 'only': return policy.accounts.includes(account);
    default: return false;
  }
}

/** A profile's accessible name, wherever its chip, pill or group stands for it. */
export const profileLabel = (p: Profile): string => `Profile ${p.name}${p.private ? ' (private)' : ''}`;

/** A profile's ∗: everything in it. */
export const allOf = (p: Profile): string => `All ${p.name}`;

/** Whether a machine entry is in a profile (shared entries are in several). */
export const machineIn = (m: Pick<Machine, 'profile_ids'>, id: ProfileId): boolean => m.profile_ids.includes(id);

/**
 * One profile's data: its machine entries (shared ones included), projects, tasks, sessions and
 * the overlaps among them. `machines` still maps every machine, so a workspace on a machine that
 * left the profile keeps its name.
 */
export function scopeToProfile(all: Ctx, id: ProfileId): Ctx {
  const sessions = all.sessions.filter((s) => s.profile_id === id);
  const ids = new Set(sessions.map((s) => s.id));
  return {
    machines: all.machines,
    machineList: all.machineList.filter((m) => machineIn(m, id)),
    projects: all.projects.filter((p) => p.profile_id === id),
    sessions,
    tasks: all.tasks.filter((t) => t.profile_id === id),
    overlaps: all.overlaps.filter((o) => o.session_ids.some((s) => ids.has(s))),
  };
}

/** The other profiles a machine entry is shared with, in order. */
export const sharedWith = (m: Pick<Machine, 'profile_ids'>, profiles: readonly Profile[], except: ProfileId | null): Profile[] =>
  profiles.filter((p) => p.id !== except && machineIn(m, p.id));

/** The archived sessions a profile's deletion takes along (an open one blocks it). */
export const archivedIn = (sessions: readonly FeedSession[], id: ProfileId): number =>
  sessions.filter((s) => s.profile_id === id && s.state === 'closed').length;

/** Why a profile can't be deleted yet, if it can't: what it still holds. */
export function deleteBlocker(all: Ctx, id: ProfileId): string | null {
  const scoped = scopeToProfile(all, id);
  const held: [number, string][] = [
    [scoped.machineList.length, 'machine'],
    [scoped.projects.length, 'project'],
    [scoped.tasks.length, 'task'],
    [scoped.sessions.filter((s) => s.state !== 'closed').length, 'open session'],
  ];
  const parts = held.filter(([n]) => n > 0).map(([n, what]) => `${n} ${what}${n === 1 ? '' : 's'}`);
  return parts.length ? `It still has ${parts.join(', ')}` : null;
}

/** Finds a profile by name: exact (any case) first, then by prefix. */
export function profileNamed(profiles: readonly Profile[], name: string): Profile | null {
  const q = name.trim().toLowerCase();
  if (!q) return null;
  return profiles.find((p) => p.name.toLowerCase() === q) ?? profiles.find((p) => p.name.toLowerCase().startsWith(q)) ?? null;
}
