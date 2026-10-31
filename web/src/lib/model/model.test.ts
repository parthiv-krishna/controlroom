import { describe, expect, it } from 'vitest';
import type { MachineSession, Project } from '../protocol';
import { loadFixture } from '../../test/fixture';
import { emptyFilters, displayName, visibleSessions } from './filters';
import { groupSessions } from './grouping';
import { loc, locOptions, locTree, normPath, parsePrefix, prefixMatch, prefixText, underLoc } from './location';
import { contentText, deepLinkHash, guessDeviceName, lineDiff, parseDeepLink } from './misc';
import { notes } from './notifications';
import { health, inProject, keyOf, needsYouElsewhere, type Ctx } from './projects';
import { defaultLevel, looser } from './sandbox';
import { humanSchedule, parseCron } from './schedule';
import { statusKind, statusLabel } from './status';
import { columnTasks, orderFor, rollup, suggestMove, taskPrompt } from './tasks';
import { leadsToMatch, treeIndex, treeRoots } from './tree';

const f = loadFixture();
const ctx: Ctx = {
  machines: new Map(f.snapshot.machines.map((m) => [m.id, m])),
  machineList: [...f.snapshot.machines].sort((a, b) => a.name.localeCompare(b.name)),
  sessions: f.snapshot.sessions,
  tasks: f.snapshot.tasks,
};
const byName = (name: string): MachineSession => {
  const s = f.snapshot.sessions.find((x) => displayName(x) === name);
  if (!s) throw new Error(name);
  return s;
};
const project = (name: string): Project => f.snapshot.projects.find((p) => p.name === name)!;
const devbox = f.snapshot.machines.find((m) => m.name === 'devbox')!;

describe('location', () => {
  it('shows home as ~ and matches subtrees', () => {
    expect(normPath('/home/alice/src/x', 'alice')).toBe('~/src/x');
    expect(normPath('/home/alice', 'alice')).toBe('~');
    expect(normPath('/srv/x', 'alice')).toBe('/srv/x');
    expect(loc({ cwd: '/home/alice/src' }, devbox)).toBe('devbox:~/src');
    expect(underLoc('devbox:~/src/a', 'devbox:~/src')).toBe(true);
    expect(underLoc('devbox:~/srcx', 'devbox:~/src')).toBe(false);
    expect(underLoc('devbox:~/src', 'devbox:')).toBe(true);
  });

  it('matches project prefixes on one machine, any machine or the whole machine', () => {
    const s = { cwd: '/home/alice/src/acme-api/crates' };
    expect(prefixMatch(s, devbox, { machine_id: null, path_prefix: '~/src/acme-api' })).toBe(true);
    expect(prefixMatch(s, devbox, { machine_id: devbox.id, path_prefix: '' })).toBe(true);
    expect(prefixMatch(s, devbox, { machine_id: 'other', path_prefix: '' })).toBe(false);
    expect(prefixMatch(s, devbox, { machine_id: null, path_prefix: '~/src/acme' })).toBe(false);
  });

  it('round-trips prefixes as text', () => {
    const p = parsePrefix('devbox:~/src', f.snapshot.machines);
    expect(p).toEqual({ machine_id: devbox.id, path_prefix: '~/src' });
    expect(prefixText(p!, ctx.machines)).toBe('devbox:~/src');
    expect(parsePrefix('~/x', [])).toEqual({ machine_id: null, path_prefix: '~/x' });
    expect(parsePrefix('nohost:/x', f.snapshot.machines)).toBeNull();
  });

  it('folds single-child chains in the location tree', () => {
    const sessions = [byName('flaky e2e tests'), byName('NixOS module hardening')];
    const opts = locOptions(locTree(sessions, f.snapshot.machines));
    expect(opts.map((o) => o.label)).toEqual(['devbox', '~/src/controlroom', 'nix', 'web']);
    expect(opts[1].value).toBe('devbox:~/src/controlroom');
  });
});

describe('status and attention', () => {
  it('maps server state to one icon language', () => {
    expect(statusKind(byName('auth token refresh race'))).toBe('attention');
    expect(statusKind(byName('flaky e2e tests'))).toBe('working');
    expect(statusKind(byName('blog: deep dive'))).toBe('asking');
    expect(statusKind(byName('NixOS module hardening'))).toBe('stopped');
    expect(statusKind(byName('migrate UI to Svelte 5 runes'))).toBe('complete');
    expect(statusKind(byName('inbox triage'))).toBe('done');
    expect(statusKind(byName('dataset dedupe'))).toBe('offline');
    expect(statusKind(byName('rotate secrets'))).toBe('archived');
    expect(statusLabel(byName('lint cleanup'))).toBe('Asleep');
    expect(statusLabel(byName('dataset dedupe'), true)).toBe('Needs approval');
  });

  it('counts stoplights from server attention, never for stale sessions', () => {
    expect(health(ctx, null)).toEqual({ approvals: 4, questions: 1, problems: 2, unread: 1 });
    expect(health(ctx, project('Docs'))).toEqual({ approvals: 1, questions: 1, problems: 0, unread: 0 });
    expect(needsYouElsewhere(ctx, project('Docs'))).toBe(5);
  });
});

describe('projects, filters and grouping', () => {
  it('puts sessions in a project by location or by a linked task', () => {
    expect(inProject(ctx, byName('auth token refresh race'), project('Acme API'))).toBe(true);
    // linked to "Release v0.2" (Controlroom) and located under ~/src/controlroom
    expect(inProject(ctx, byName('lint cleanup'), project('Controlroom'))).toBe(true);
    expect(inProject(ctx, byName('inbox triage'), project('Controlroom'))).toBe(false);
  });

  it('hides archived sessions unless filtered for or kept', () => {
    const all = visibleSessions(ctx, null, emptyFilters(), '', new Set());
    expect(all.some((s) => s.state === 'closed')).toBe(false);
    expect(all[0].attention).toBe('approval');
    const archived = visibleSessions(ctx, null, { ...emptyFilters(), state: ['closed'] }, '', new Set());
    expect(archived.map(displayName)).toEqual(['rotate secrets']);
    const kept = visibleSessions(ctx, null, emptyFilters(), '', new Set([keyOf(byName('rotate secrets'))]));
    expect(kept.map(displayName)).toContain('rotate secrets');
  });

  it('searches names, locations and task titles', () => {
    expect(visibleSessions(ctx, null, emptyFilters(), 'logouts', new Set()).map(displayName)).toEqual(['auth token refresh race', 'upgrade tokio to 1.47']);
    expect(visibleSessions(ctx, null, emptyFilters(), 'gpubox:', new Set()).map(displayName)).toEqual(['dataset dedupe']);
  });

  it('filters by facet', () => {
    const fl = { ...emptyFilters(), machine: [devbox.id], state: ['needs'] };
    expect(visibleSessions(ctx, null, fl, '', new Set()).map(displayName).sort()).toEqual(['NixOS module hardening', 'docs site refresh', 'sweep: 64k–128k'].sort());
  });

  it('groups by task with empty unstarted tasks unless filtering', () => {
    const list = visibleSessions(ctx, project('Docs'), emptyFilters(), '', new Set());
    const groups = groupSessions(ctx, project('Docs'), list, 'task', {}, false);
    expect(groups.map((g) => [g.label, g.items.length])).toEqual([['Docs refresh', 2], ['Write onboarding guide', 0]]);
    expect(groupSessions(ctx, project('Docs'), list, 'task', {}, true).map((g) => g.label)).toEqual(['Docs refresh']);
  });

  it('groups by location as a nested tree with "new session here"', () => {
    const list = [byName('flaky e2e tests'), byName('NixOS module hardening')];
    const groups = groupSessions(ctx, null, list, 'location', {}, false);
    expect(groups.map((g) => [g.label, g.depth])).toEqual([['devbox', 0], ['~/src/controlroom', 1], ['nix', 2], ['web', 2]]);
    expect(groups[2].newAt).toEqual({ machine_id: devbox.id, cwd: '~/src/controlroom/nix' });
    const folded = groupSessions(ctx, null, list, 'location', { [groups[1].key]: true }, false);
    expect(folded).toHaveLength(2);
  });

  it('groups by state and host', () => {
    const list = visibleSessions(ctx, null, emptyFilters(), '', new Set());
    expect(groupSessions(ctx, null, list, 'state', {}, false).map((g) => g.label)).toEqual([
      'Needs a decision', 'Failed or stopped', 'Open questions', 'Unread results', 'Working', 'Idle', 'Disconnected',
    ]);
    const hosts = groupSessions(ctx, null, list, 'machine', {}, false);
    expect(hosts.map((g) => g.label)).toEqual(['buildbox', 'devbox', 'gpubox']);
    expect(hosts[2].newAt).toBeUndefined();
  });
});

describe('subagent tree', () => {
  it('nests children and keeps a dimmed ancestor when only a child matches', () => {
    const all = f.snapshot.sessions;
    const idx = treeIndex(all, new Set());
    const coord = byName('benchmark coordinator');
    expect(idx.children(coord).map(displayName)).toEqual(['sweep: 64k–128k', 'sweep: 8k–32k']);
    const child = byName('sweep: 64k–128k');
    const visible = new Set([keyOf(child)]);
    expect(treeRoots([child], visible, idx)).toEqual([{ s: coord, dim: true }]);
    expect(idx.children(coord).filter((c) => leadsToMatch(c, visible, idx)).map(displayName)).toEqual(['sweep: 64k–128k']);
    // when the parent itself is visible it renders the child; no second root
    expect(treeRoots([coord, child], new Set([keyOf(coord), keyOf(child)]), idx)).toEqual([{ s: coord, dim: false }]);
  });
});

describe('notifications', () => {
  it('lists each request once, under the top-level session', () => {
    const n = notes(ctx, null);
    expect(n.blocking.map((x) => x.req.title)).toContain('Run ./sweep.sh --ctx 64k..128k?');
    expect(n.blocking.filter((x) => x.req.title === 'Run ./sweep.sh --ctx 64k..128k?')).toHaveLength(1);
    expect(n.blocking.find((x) => x.req.origin_child)?.s.name).toBe('benchmark coordinator');
    expect(n.questions.map((x) => x.req.kind)).toEqual(['question']);
    expect(n.problems.map(displayName)).toEqual(['NixOS module hardening', 'nightly data pipeline']);
    expect(n.finished.map(displayName)).toEqual(['migrate UI to Svelte 5 runes']);
    expect(n.offline.map((o) => [o.machine.name, o.waiting])).toEqual([['gpubox', 1]]);
  });
});

describe('tasks', () => {
  const doing = columnTasks(f.snapshot.tasks, 'doing');

  it('computes fractional orders for moves', () => {
    expect(doing.map((t) => t.order)).toEqual([1, 2, 5]);
    expect(orderFor(doing, 'x', null)).toBe(6);
    expect(orderFor(doing, 'x', doing[1].id)).toBe(1.5);
    expect(orderFor(doing, 'x', doing[0].id)).toBe(0);
    expect(orderFor(doing, doing[2].id, doing[2].id)).toBe(3);
    expect(orderFor([], 'x', null)).toBe(1);
  });

  it('rolls up linked sessions, keeping missing ones', () => {
    const done = f.snapshot.tasks.find((t) => t.title === 'Rotate secrets')!;
    expect(rollup(ctx, done).sessions.map((s) => s && displayName(s))).toEqual(['rotate secrets', null]);
    const logouts = f.snapshot.tasks.find((t) => t.title === 'Fix random logouts')!;
    expect(rollup(ctx, logouts)).toMatchObject({ approvals: 2, working: 0 });
  });

  it('suggests moves without applying them', () => {
    const docs = f.snapshot.tasks.find((t) => t.title === 'Docs refresh')!;
    expect(suggestMove(ctx, docs)).toEqual({ to: 'doing', why: 'a linked agent is active' });
    expect(suggestMove(ctx, doing[0])).toBeNull();
  });

  it('writes the first message for a task', () => {
    const t = f.snapshot.tasks.find((x) => x.title === 'Fix random logouts')!;
    expect(taskPrompt(t)).toContain('- [x] Reproduce');
  });
});

describe('schedules', () => {
  it('describes cron and interval specs', () => {
    expect(humanSchedule({ kind: 'cron', expr: '0 9 * * 1-5' })).toBe('Weekdays 09:00');
    expect(humanSchedule({ kind: 'cron', expr: '*/15 * * * *' })).toBe('Every 15 min');
    expect(humanSchedule({ kind: 'cron', expr: '30 8 * * 0,6' })).toBe('Weekends 08:30');
    expect(humanSchedule({ kind: 'cron', expr: '0 7 1 * *' })).toBe('Monthly on day 1 07:00');
    expect(humanSchedule({ kind: 'interval', every_s: 21600 })).toBe('Every 6 h');
    expect(humanSchedule({ kind: 'interval', every_s: 1800 })).toBe('Every 30 min');
    expect(humanSchedule({ kind: 'cron', expr: 'nope' })).toContain('invalid');
    expect(parseCron('61 * * * *')).toBeNull();
  });
});

describe('sandbox defaults', () => {
  const profiles = f.responses['sandbox.profiles']!.profiles;
  const buildbox = f.snapshot.machines.find((m) => m.name === 'buildbox')!;

  it('previews the strictest level of the machine and matching projects', () => {
    expect(defaultLevel(profiles, buildbox, '/home/alice/data', f.snapshot.projects)).toBe('relaxed');
    expect(defaultLevel(profiles, buildbox, '/home/alice/src/acme-api', f.snapshot.projects)).toBe('strict');
    expect(defaultLevel([], devbox, '/x', [])).toBe('strict');
    expect(looser('off', 'relaxed')).toBe(true);
    expect(looser('strict', 'relaxed')).toBe(false);
  });
});

describe('misc', () => {
  it('guesses device names', () => {
    expect(guessDeviceName('Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0')).toBe('Firefox on Linux');
    expect(guessDeviceName('Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Version/18.0 Mobile/15E148 Safari/604.1')).toBe('Safari on iPhone');
    expect(guessDeviceName('')).toBe('Browser');
  });

  it('round-trips deep links with their pre-render params', () => {
    expect(parseDeepLink(deepLinkHash('m 1', 's'))).toMatchObject({ machine_id: 'm 1', id: 's', name: null });
    expect(parseDeepLink('#/s/m/s?n=Fix%20it&m=devbox&st=Needs%20approval')).toEqual({ machine_id: 'm', id: 's', name: 'Fix it', machine: 'devbox', state: 'Needs approval' });
    expect(parseDeepLink('#/other')).toBeNull();
    expect(parseDeepLink('#/s/%E0%A4%A/s')).toBeNull();
  });

  it('diffs lines', () => {
    expect(lineDiff('a\nb\nc', 'a\nc\nd')).toEqual([
      { kind: 'ctx', text: 'a' }, { kind: 'del', text: 'b' }, { kind: 'ctx', text: 'c' }, { kind: 'add', text: 'd' },
    ]);
    expect(lineDiff('', 'x')).toEqual([{ kind: 'add', text: 'x' }]);
  });

  it('extracts prompt text', () => {
    expect(contentText([{ kind: 'text', text: 'hi' }, { kind: 'image', mime_type: 'image/png', data: '' }])).toBe('hi [image]');
  });
});
