import { describe, expect, it } from 'vitest';
import type { FeedSession, Project } from '../protocol';
import { loadFixture } from '../../test/fixture';
import { emptyFilters, displayName, visibleSessions } from './filters';
import { groupSessions } from './grouping';
import { contentText, deepLinkHash, guessDeviceName, lineDiff, parseDeepLink } from './misc';
import { notes } from './notifications';
import { UNASSIGNED, health, inScope, needsYouElsewhere, taskIn, type Ctx } from './projects';
import { defaultLevel, looser } from './sandbox';
import { humanSchedule, inTime, parseCron, runText, runWhen } from './schedule';
import { connKind, connLabel, statusKind, statusLabel, statusTitle } from './status';
import { columnTasks, orderFor, rollup, suggestMove, taskPrompt } from './tasks';
import { leadsToMatch, treeIndex, treeRoots } from './tree';
import { availabilityText, machinesOf, normPath, overlapsOf, primaryWorkspace, splitMachinePaths, touches, unavailable, wsLabel } from './workspaces';

const f = loadFixture();
const ctx: Ctx = {
  machines: new Map(f.snapshot.machines.map((m) => [m.id, m])),
  machineList: [...f.snapshot.machines].sort((a, b) => a.name.localeCompare(b.name)),
  sessions: f.snapshot.sessions,
  tasks: f.snapshot.tasks,
  overlaps: f.snapshot.overlaps,
};
const byName = (name: string): FeedSession => {
  const s = f.snapshot.sessions.find((x) => displayName(x) === name);
  if (!s) throw new Error(name);
  return s;
};
const project = (name: string): Project => f.snapshot.projects.find((p) => p.name === name)!;
const machine = (name: string) => f.snapshot.machines.find((m) => m.name === name)!;
const devbox = machine('devbox');
const buildbox = machine('buildbox');
const gpubox = machine('gpubox');

describe('workspaces', () => {
  it('shows home as ~ and names directories machine:path', () => {
    expect(normPath('/home/alice/src/x', 'alice')).toBe('~/src/x');
    expect(normPath('/home/alice', 'alice')).toBe('~');
    expect(normPath('/srv/x', 'alice')).toBe('/srv/x');
    const race = byName('auth token refresh race');
    expect(race.workspaces.map((w) => wsLabel(w, ctx.machines))).toEqual(['buildbox:~/src/acme-api', 'devbox:~/src/acme-web']);
    expect(primaryWorkspace(race)?.machine_id).toBe(buildbox.id);
    expect(machinesOf(race)).toEqual([buildbox.id, devbox.id]);
    expect(touches(race, devbox.id)).toBe(true);
    expect(touches(race, gpubox.id)).toBe(false);
  });

  it('reports the availability of each workspace; the session itself never goes stale', () => {
    const dedupe = byName('dataset dedupe');
    expect(unavailable(dedupe).map((w) => w.machine_id)).toEqual([gpubox.id]);
    const now = dedupe.workspaces[0].availability.since + 2 * 3600e3;
    expect(availabilityText(dedupe.workspaces[0], ctx.machines, now)).toBe('gpubox unreachable since 2h ago');
    expect(availabilityText(dedupe.workspaces[1], ctx.machines, now)).toBe('devbox connected');
    // Still counts as needing approval: its requests are answerable on the server.
    expect(statusKind(dedupe)).toBe('attention');
    expect(statusTitle(dedupe, ctx.machines)).toBe('Needs approval · gpubox unreachable');
  });

  it('finds the overlaps a session is part of', () => {
    const flaky = byName('flaky e2e tests');
    expect(overlapsOf(flaky, ctx.overlaps)).toEqual([
      { machine_id: devbox.id, path: '/home/alice/src/controlroom/web', session_ids: [flaky.id, byName('migrate UI to Svelte 5 runes').id] },
    ]);
    expect(overlapsOf(byName('inbox triage'), ctx.overlaps)).toEqual([]);
  });

  it('splits machine:path runs out of text for known machines only', () => {
    expect(splitMachinePaths('Read devbox:src/main.rs and x:y', ['devbox'])).toEqual([
      { text: 'Read ' },
      { text: 'devbox:src/main.rs', machine: 'devbox', path: 'src/main.rs' },
      { text: ' and x:y' },
    ]);
    expect(splitMachinePaths('mydevbox:~/a', ['devbox'])).toEqual([{ text: 'mydevbox:~/a' }]);
    expect(splitMachinePaths('"buildbox:~/w/a.js",', ['devbox', 'buildbox'])[1]).toEqual({ text: 'buildbox:~/w/a.js', machine: 'buildbox', path: '~/w/a.js' });
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
    expect(statusKind(byName('rotate secrets'))).toBe('archived');
    expect(statusLabel(byName('lint cleanup'))).toBe('Asleep');
    expect(statusLabel(byName('e2e flake hunt'))).toBe('Needs answer');
  });

  it('counts stoplights from server attention per scope', () => {
    expect(health(ctx, null)).toEqual({ approvals: 7, questions: 1, problems: 2, unread: 1 });
    expect(health(ctx, project('Docs'))).toEqual({ approvals: 1, questions: 1, problems: 0, unread: 0 });
    expect(health(ctx, UNASSIGNED)).toEqual({ approvals: 3, questions: 0, problems: 2, unread: 1 });
    expect(needsYouElsewhere(ctx, project('Docs'))).toBe(8);
  });

  it('labels every reason a machine needs the user', () => {
    const with_ = (reason: string) =>
      ({ ...devbox, connectivity: { state: 'action_required', reason, presented_host_key: null } }) as typeof devbox;
    expect(connLabel(with_('controller_conflict'))).toBe('another server connected');
    expect(connLabel(with_('auth_failed'))).toBe('key rejected');
    expect(connLabel(with_('host_key_unconfirmed'))).toBe('confirm host key');
    expect(connLabel(with_('unknown'))).toBe('needs attention');
    expect(connKind(with_('controller_conflict'))).toBe('attention');
  });
});

describe('projects, filters and grouping', () => {
  it('puts sessions in the projects of their linked tasks, else Unassigned', () => {
    expect(inScope(byName('auth token refresh race'), project('Acme API'))).toBe(true);
    expect(inScope(byName('lint cleanup'), project('Controlroom'))).toBe(true);
    expect(inScope(byName('inbox triage'), project('Controlroom'))).toBe(false);
    expect(inScope(byName('inbox triage'), UNASSIGNED)).toBe(true);
    expect(inScope(byName('auth token refresh race'), UNASSIGNED)).toBe(false);
    expect(inScope(byName('inbox triage'), null)).toBe(true);
    const unassigned = visibleSessions(ctx, UNASSIGNED, emptyFilters(), '', new Set()).map(displayName).sort();
    expect(unassigned).toEqual(
      ['NixOS module hardening', 'dataset dedupe', 'inbox triage', 'legacy deploy scripts', 'migrate UI to Svelte 5 runes', 'nightly data pipeline', 'sweep: 64k–128k', 'sweep: 8k–32k'].sort(),
    );
    const orphan = f.snapshot.tasks.find((t) => t.title === 'Rotate secrets')!;
    expect(taskIn(orphan, UNASSIGNED)).toBe(true);
    expect(taskIn(orphan, project('Docs'))).toBe(false);
  });

  it('hides archived sessions unless filtered for or kept', () => {
    const all = visibleSessions(ctx, null, emptyFilters(), '', new Set());
    expect(all.some((s) => s.state === 'closed')).toBe(false);
    expect(all[0].attention).toBe('approval');
    const archived = visibleSessions(ctx, null, { ...emptyFilters(), state: ['closed'] }, '', new Set());
    expect(archived.map(displayName)).toEqual(['rotate secrets']);
    const kept = visibleSessions(ctx, null, emptyFilters(), '', new Set([byName('rotate secrets').id]));
    expect(kept.map(displayName)).toContain('rotate secrets');
  });

  it('searches names, every workspace and task titles', () => {
    expect(visibleSessions(ctx, null, emptyFilters(), 'logouts', new Set()).map(displayName)).toEqual(['auth token refresh race', 'upgrade tokio to 1.47']);
    expect(visibleSessions(ctx, null, emptyFilters(), 'gpubox:', new Set()).map(displayName)).toEqual(['dataset dedupe']);
    // The second workspace counts too.
    expect(visibleSessions(ctx, null, emptyFilters(), 'devbox:~/src/acme-web', new Set()).map(displayName)).toEqual(['auth token refresh race']);
  });

  it('filters by the machines a session touches, and by state', () => {
    const fl = { ...emptyFilters(), machine: [buildbox.id], state: ['needs'] };
    expect(visibleSessions(ctx, null, fl, '', new Set()).map(displayName).sort()).toEqual(['auth token refresh race', 'e2e flake hunt', 'nightly data pipeline', 'upgrade tokio to 1.47'].sort());
    const away = visibleSessions(ctx, null, { ...emptyFilters(), state: ['unreachable'] }, '', new Set());
    expect(away.map(displayName)).toEqual(['dataset dedupe']);
  });

  it('groups by task (the default) with empty unstarted tasks unless filtering', () => {
    const list = visibleSessions(ctx, project('Docs'), emptyFilters(), '', new Set());
    const groups = groupSessions(ctx, project('Docs'), list, 'task', false);
    expect(groups.map((g) => [g.label, g.items.length])).toEqual([['Docs refresh', 2], ['Write onboarding guide', 0]]);
    expect(groupSessions(ctx, project('Docs'), list, 'task', true).map((g) => g.label)).toEqual(['Docs refresh']);
    const unassigned = visibleSessions(ctx, UNASSIGNED, emptyFilters(), '', new Set());
    expect(groupSessions(ctx, UNASSIGNED, unassigned, 'task', false).map((g) => g.label)).toEqual(['Not on a task']);
  });

  it('groups by machine: a session is under every machine it touches', () => {
    const list = visibleSessions(ctx, null, emptyFilters(), '', new Set());
    const hosts = groupSessions(ctx, null, list, 'machine', false);
    expect(hosts.map((g) => g.label)).toEqual(['buildbox', 'devbox', 'gpubox']);
    const race = byName('auth token refresh race');
    expect(hosts.filter((g) => g.items.includes(race)).map((g) => g.label)).toEqual(['buildbox', 'devbox']);
    expect(hosts[2].items.map(displayName)).toEqual(['dataset dedupe']);
  });

  it('groups by state, listing sessions waiting on an unreachable machine together', () => {
    const list = visibleSessions(ctx, null, emptyFilters(), '', new Set());
    const groups = groupSessions(ctx, null, list, 'state', false);
    expect(groups.map((g) => g.label)).toEqual(['Needs a decision', 'Failed or stopped', 'Open questions', 'Unread results', 'Working', 'Idle', 'Machine unreachable']);
    expect(groups.at(-1)!.items.map(displayName)).toEqual(['dataset dedupe']);
  });
});

describe('subagent tree', () => {
  it('nests children and keeps a dimmed ancestor when only a child matches', () => {
    const all = f.snapshot.sessions;
    const idx = treeIndex(all, new Set());
    const coord = byName('benchmark coordinator');
    expect(idx.children(coord).map(displayName)).toEqual(['sweep: 64k–128k', 'sweep: 8k–32k']);
    const child = byName('sweep: 64k–128k');
    const visible = new Set([child.id]);
    expect(treeRoots([child], visible, idx)).toEqual([{ s: coord, dim: true }]);
    expect(idx.children(coord).filter((c) => leadsToMatch(c, visible, idx)).map(displayName)).toEqual(['sweep: 64k–128k']);
    // when the parent itself is visible it renders the child; no second root
    expect(treeRoots([coord, child], new Set([coord.id, child.id]), idx)).toEqual([{ s: coord, dim: false }]);
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
    expect(n.offline.map((o) => [o.machine.name, o.sessions])).toEqual([['gpubox', 1]]);
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

  it('accepts month and weekday names', () => {
    expect(humanSchedule({ kind: 'cron', expr: '0 9 * * mon-fri' })).toBe('Weekdays 09:00');
    expect(humanSchedule({ kind: 'cron', expr: '0 16 * * FRI' })).toBe('Fridays 16:00');
    expect(parseCron('0 9 1 JAN *')).not.toBeNull();
    expect(parseCron('0 9 * * FUNDAY')).toBeNull();
  });

  it('shows runs, collapsed misses and the time until the next run', () => {
    const [fired, missed] = f.snapshot.schedules[0].recent_runs;
    expect(runText(fired)).toBe('fired');
    expect(runText(missed)).toBe('missed ×3');
    expect(runWhen(missed, 'UTC')).toBe('Fri 08:00 – Sun 08:00');
    const now = 1_767_600_000_000;
    expect(inTime(now + 20e3, now)).toBe('now');
    expect(inTime(now + 5 * 60e3, now)).toBe('in 5m');
    expect(inTime(now + 3 * 3600e3, now)).toBe('in 3h');
    expect(inTime(now + 3 * 86400e3, now)).toBe('in 3d');
  });
});

describe('sandbox defaults', () => {
  const profiles = f.responses['sandbox.profiles']!.profiles;

  it('previews the strictest level of the workspaces\' machines and the tasks\' projects', () => {
    expect(defaultLevel(profiles, [buildbox.id], [])).toBe('relaxed');
    expect(defaultLevel(profiles, [buildbox.id], [project('Acme API').id])).toBe('strict');
    expect(defaultLevel(profiles, [buildbox.id, devbox.id], [])).toBe('strict');
    expect(defaultLevel([], [devbox.id], [])).toBe('strict');
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
    expect(parseDeepLink(deepLinkHash('s 1'))).toMatchObject({ id: 's 1', name: null });
    expect(parseDeepLink('#/s/s?n=Fix%20it&m=devbox&st=Needs%20approval')).toEqual({ id: 's', name: 'Fix it', machine: 'devbox', state: 'Needs approval' });
    expect(parseDeepLink('#/other')).toBeNull();
    expect(parseDeepLink('#/s/m/s')).toBeNull();
    expect(parseDeepLink('#/s/%E0%A4%A')).toBeNull();
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
