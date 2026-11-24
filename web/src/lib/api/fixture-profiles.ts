// Profile variants of the development fixture (fixture build only): `?profiles=` picks one.
// The committed fixture has one profile that owns everything. `0` is a fresh install (no
// profiles yet), `legacy` a server before profiles (`profiles.*` unsupported), and `3` reassigns
// machines, projects, tasks and sessions to three profiles (web/fixtures/profiles.json).
import type { MachineId, Profile, ProfileId, ProjectId, SandboxDefaults, SessionId, TaskId } from '../protocol';
import type { Fixture } from './fixture';

/** A multi-profile assignment of the fixture's objects (unlisted ones keep the first profile). */
export interface ProfileSpec {
  profiles: Profile[];
  /** Each machine's profiles (several: shared). */
  machines: Record<MachineId, ProfileId[]>;
  projects: Record<ProjectId, ProfileId>;
  /** Tasks whose profile isn't the first one. */
  tasks: Record<TaskId, ProfileId>;
  /** Sessions whose profile isn't the first one. */
  sessions: Record<SessionId, ProfileId>;
  /** Sessions whose workspaces move from one machine to another: `[from, to]`. */
  workspaces: Record<SessionId, [MachineId, MachineId]>;
  /** Sandbox defaults of profile scope, added to the fixture's. */
  defaults: SandboxDefaults[];
}

export type ProfileVariant = 'one' | 'none' | 'legacy' | ProfileSpec;

/** The variant a `?profiles=` value names; anything else keeps the committed fixture. */
export function profileVariant(value: string | null, three: ProfileSpec): ProfileVariant {
  if (value === '0') return 'none';
  if (value === 'legacy') return 'legacy';
  if (value === '3') return three;
  return 'one';
}

/** The fixture with its profiles replaced as the variant says (a copy; the input is untouched). */
export function withProfiles(source: Fixture, variant: ProfileVariant): Fixture {
  if (variant === 'one') return source;
  const f = structuredClone(source);
  if (variant === 'none' || variant === 'legacy') {
    f.snapshot.profiles = [];
    for (const m of f.snapshot.machines) m.profile_ids = [];
    for (const p of f.snapshot.projects) p.profile_id = null;
    for (const t of f.snapshot.tasks) t.profile_id = null;
    for (const s of f.snapshot.sessions) s.profile_id = null;
    if (variant === 'legacy') delete f.responses['profiles.list'];
    else f.responses['profiles.list'] = { profiles: [] };
    return f;
  }
  const first = variant.profiles[0].id;
  f.snapshot.profiles = variant.profiles;
  f.responses['profiles.list'] = { profiles: variant.profiles };
  for (const m of f.snapshot.machines) m.profile_ids = variant.machines[m.id] ?? [first];
  for (const p of f.snapshot.projects) p.profile_id = variant.projects[p.id] ?? first;
  for (const t of f.snapshot.tasks) t.profile_id = variant.tasks[t.id] ?? first;
  for (const s of f.snapshot.sessions) {
    s.profile_id = variant.sessions[s.id] ?? first;
    const moved = variant.workspaces[s.id];
    if (moved) for (const w of s.workspaces) if (w.machine_id === moved[0]) w.machine_id = moved[1];
  }
  const defaults = f.responses['sandbox.defaults'];
  if (defaults) f.responses['sandbox.defaults'] = { defaults: [...defaults.defaults, ...variant.defaults] };
  return f;
}
