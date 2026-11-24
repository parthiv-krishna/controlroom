import { describe, expect, it } from 'vitest';
import three from '../../../fixtures/profiles.json';
import { loadFixture } from '../../test/fixture';
import { withProfiles, type ProfileSpec } from '../api/fixture-profiles';
import type { Profile, SandboxDefaults } from '../protocol';
import { notes } from './notifications';
import { admits, allOf, archivedIn, deleteBlocker, machineIn, profileLabel, profileNamed, scopeToProfile, sharedWith, sortProfiles } from './profiles';
import { health, needsYouElsewhere, type Ctx } from './projects';
import { defaultLevel } from './sandbox';

// SAFETY: JSON imports widen string unions; the consistency test below checks the spec.
const spec = three.three as unknown as ProfileSpec;
const f = withProfiles(loadFixture(), spec);
const all: Ctx = {
  machines: new Map(f.snapshot.machines.map((m) => [m.id, m])),
  machineList: [...f.snapshot.machines].sort((a, b) => a.name.localeCompare(b.name)),
  projects: f.snapshot.projects,
  sessions: f.snapshot.sessions,
  tasks: f.snapshot.tasks,
  overlaps: f.snapshot.overlaps,
};
const [orchard, lantern, quill] = spec.profiles;
const names = (xs: readonly { name: string }[]) => xs.map((x) => x.name);

describe('profiles', () => {
  it('sorts by position, then name', () => {
    const p = (name: string, position: number): Profile => ({ ...orchard, id: name, name, position });
    expect(names(sortProfiles([p('b', 2), p('c', 1), p('a', 2)]))).toEqual(['c', 'a', 'b']);
  });

  it('admits accounts by policy, and nothing when the policy is unknown', () => {
    expect(admits({ kind: 'any' }, 'anything')).toBe(true);
    expect(admits({ kind: 'only', accounts: ['openai'] }, 'openai')).toBe(true);
    expect(admits({ kind: 'only', accounts: ['openai'] }, 'openai-work')).toBe(false);
    expect(admits({ kind: 'unknown' }, 'openai')).toBe(false);
  });

  it('names profiles for chips, pickers and ∗', () => {
    expect(profileLabel(orchard)).toBe('Profile Orchard');
    expect(profileLabel(quill)).toBe('Profile Quill (private)');
    expect(allOf(lantern)).toBe('All Lantern');
  });

  it('scopes everything to one profile; shared machines are in each', () => {
    const l = scopeToProfile(all, lantern.id);
    expect(names(l.machineList)).toEqual(['devbox']);
    expect(names(l.projects)).toEqual(['Docs']);
    expect(l.tasks.map((t) => t.title).sort()).toEqual(['Docs refresh', 'Write onboarding guide']);
    expect(l.sessions.every((s) => s.profile_id === lantern.id)).toBe(true);
    expect(l.overlaps).toEqual([]);
    // Every machine keeps its name for lookups.
    expect(l.machines.size).toBe(all.machines.size);
    const o = scopeToProfile(all, orchard.id);
    expect(names(o.machineList)).toEqual(['buildbox', 'devbox', 'gpubox']);
    expect(o.overlaps).toHaveLength(3);
    expect(names(scopeToProfile(all, quill.id).machineList)).toEqual(['armbox', 'oldbox']);
    const devbox = all.machineList.find((m) => m.name === 'devbox')!;
    expect(machineIn(devbox, lantern.id)).toBe(true);
    expect(names(sharedWith(devbox, spec.profiles, orchard.id))).toEqual(['Lantern']);
  });

  it('counts attention per profile; the rest is elsewhere', () => {
    const l = scopeToProfile(all, lantern.id);
    const q = scopeToProfile(all, quill.id);
    const sum = (c: Ctx) => Object.values(health(c, null)).reduce((a, b) => a + b, 0);
    expect(sum(scopeToProfile(all, orchard.id)) + sum(l) + sum(q)).toBe(sum(all));
    const needs = (c: Ctx) => health(c, null).approvals + health(c, null).problems;
    expect(needsYouElsewhere(all, l, null)).toBe(needs(all) - needs(l));
    expect(needsYouElsewhere(all, l, null)).toBeGreaterThan(0);
    expect(needsYouElsewhere(all, all, null)).toBe(0);
    // Notifications under ∗ aggregate every profile.
    expect(notes(all, null).total).toBe(notes(scopeToProfile(all, orchard.id), null).total + notes(l, null).total + notes(q, null).total);
  });

  it('says what blocks deleting a profile, and counts the archived sessions it takes along', () => {
    expect(deleteBlocker(all, lantern.id)).toBe('It still has 1 machine, 1 project, 2 tasks, 3 open sessions');
    const empty: Profile = { ...orchard, id: '01JAF000000000000000000009' };
    expect(deleteBlocker(all, empty.id)).toBeNull();
    expect(archivedIn(all.sessions, orchard.id)).toBe(1);
    expect(archivedIn(all.sessions, lantern.id)).toBe(0);
  });

  it('finds a profile by name, exact first', () => {
    const similar = [{ ...orchard, name: 'Work stuff' }, { ...lantern, name: 'Work' }];
    expect(profileNamed(similar, 'work')?.id).toBe(lantern.id);
    expect(profileNamed(similar, 'work s')?.id).toBe(orchard.id);
    expect(profileNamed(similar, 'x')).toBeNull();
    expect(profileNamed(similar, ' ')).toBeNull();
  });

  it("counts the profile's sandbox level in a new session's default", () => {
    const d = (scope: SandboxDefaults['scope'], level: SandboxDefaults['level']): SandboxDefaults => ({ scope, level, grants: [], domains: [], mediated: [], never: [] });
    const machine = all.machineList[0].id;
    const defaults = [d({ kind: 'profile', profile_id: lantern.id }, 'relaxed'), d({ kind: 'machine', machine_id: machine }, 'off')];
    expect(defaultLevel(defaults, [machine], [], lantern.id)).toBe('relaxed');
    expect(defaultLevel(defaults, [machine], [], orchard.id)).toBe('off');
    expect(defaultLevel(defaults, [], [], lantern.id)).toBe('relaxed');
    expect(defaultLevel(defaults, [], [])).toBe('strict');
  });
});

describe('the three-profile fixture', () => {
  it('keeps every object inside its profile, as the server would', () => {
    const machines = new Map(f.snapshot.machines.map((m) => [m.id, m]));
    const projects = new Map(f.snapshot.projects.map((p) => [p.id, p]));
    const sessions = new Map(f.snapshot.sessions.map((s) => [s.id, s]));
    for (const s of f.snapshot.sessions) {
      for (const w of s.workspaces) expect(machines.get(w.machine_id)?.profile_ids, `${s.id} on ${w.machine_id}`).toContain(s.profile_id);
      if (s.parent_id) expect(sessions.get(s.parent_id)?.profile_id).toBe(s.profile_id);
    }
    for (const t of f.snapshot.tasks) {
      for (const p of t.project_ids) expect(projects.get(p)?.profile_id).toBe(t.profile_id);
      for (const id of t.links) if (sessions.has(id)) expect(sessions.get(id)?.profile_id).toBe(t.profile_id);
    }
    for (const g of f.snapshot.grants) expect(sessions.get(g.from)?.profile_id).toBe(sessions.get(g.to)?.profile_id);
    // A private profile's machines are never shared.
    for (const m of f.snapshot.machines) if (m.profile_ids.includes(quill.id)) expect(m.profile_ids).toEqual([quill.id]);
    expect(f.responses['profiles.list']?.profiles).toEqual(spec.profiles);
  });

  it('leaves the committed fixture alone, and strips profiles for a fresh install', () => {
    const base = loadFixture();
    expect(base.snapshot.profiles).toHaveLength(1);
    expect(base.snapshot.sessions.every((s) => s.profile_id === base.snapshot.profiles[0].id)).toBe(true);
    const none = withProfiles(base, 'none');
    expect(none.snapshot.profiles).toEqual([]);
    expect(none.snapshot.machines.every((m) => !m.profile_ids.length)).toBe(true);
    expect(none.responses['profiles.list']).toEqual({ profiles: [] });
    expect(withProfiles(base, 'legacy').responses['profiles.list']).toBeUndefined();
    expect(base.snapshot.profiles).toHaveLength(1);
  });
});
