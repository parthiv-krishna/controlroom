import { describe, expect, it } from 'vitest';
import { loadFixture } from '../../test/fixture';
import { FixtureTransport } from './fixture';

const open = async (t: FixtureTransport) => {
  await Promise.resolve();
  expect(t.state).toBe('open');
};

describe('FixtureTransport', () => {
  it('serves the snapshot and session details', async () => {
    const fixture = loadFixture();
    const t = new FixtureTransport(fixture, { autoplay: false });
    await open(t);
    const r = await t.request('state.subscribe', { since: null });
    expect(r.snapshot?.sessions).toHaveLength(fixture.snapshot.sessions.length);
    const key = Object.keys(fixture.details)[0];
    const [machine_id, id] = key.split('/');
    const sub = await t.request('session.subscribe', { machine_id, id, since_seq: 0 });
    expect(sub.detail).toEqual(fixture.details[key]);
  });

  it('records mutations and answers acks or canned results', async () => {
    const t = new FixtureTransport(loadFixture(), { autoplay: false });
    await open(t);
    await expect(t.request('session.cancel', { machine_id: 'm', op_id: 'o', id: 's' })).resolves.toEqual({});
    await expect(t.request('session.prompt', { machine_id: 'm', op_id: 'o', id: 's', content: [], origin: { kind: 'user', device: null }, if_busy: 'queue' })).resolves.toEqual({ outcome: 'sent' });
    expect(t.calls.map((c) => c.method)).toEqual(['session.cancel', 'session.prompt']);
  });

  it('fails methods it has no result for', async () => {
    const fixture = loadFixture();
    delete fixture.responses['keys.list'];
    const t = new FixtureTransport(fixture, { autoplay: false });
    await open(t);
    await expect(t.request('keys.list', {})).rejects.toMatchObject({ data: { kind: 'unsupported' } });
  });

  it('plays scripted events with the subscription ids it handed out', async () => {
    const fixture = loadFixture();
    const t = new FixtureTransport(fixture, { autoplay: false });
    await open(t);
    const seen: string[] = [];
    t.subscribe('state.events', (p) => seen.push(p.subscription_id));
    const { subscription_id } = await t.request('state.subscribe', { since: null });
    expect(t.step()).toBe(true);
    expect(seen).toEqual([subscription_id]);
  });

  it('stays offline when asked', async () => {
    const t = new FixtureTransport(loadFixture(), { offline: true });
    await Promise.resolve();
    expect(t.state).toBe('closed');
    await expect(t.request('hello', { protocol_version: { major: 1, minor: 0 } })).rejects.toMatchObject({ reason: 'offline' });
  });
});

describe('the development fixture', () => {
  const f = loadFixture();
  const machines = new Set(f.snapshot.machines.map((m) => m.id));
  const sessions = new Set(f.snapshot.sessions.map((s) => `${s.machine_id}/${s.id}`));

  it('references only known machines, sessions and projects', () => {
    for (const s of f.snapshot.sessions) expect(machines).toContain(s.machine_id);
    for (const key of Object.keys(f.details)) expect(sessions).toContain(key);
    const projects = new Set(f.snapshot.projects.map((p) => p.id));
    for (const t of f.snapshot.tasks) for (const p of t.project_ids) expect(projects).toContain(p);
    for (const x of f.snapshot.schedules) expect(sessions).toContain(`${x.machine_id}/${x.session_id}`);
  });

  it('continues the feed from the cursor', () => {
    const revs = f.script.flatMap((s) => (s.notification === 'state.events' ? s.params.changes.map((c) => c.rev) : []));
    revs.forEach((rev, i) => expect(rev).toBe(f.cursor.rev + i + 1));
  });

  it('contains no real host names', () => {
    for (const m of f.snapshot.machines) expect(m.host).toMatch(/\.example\.com$/);
  });
});
