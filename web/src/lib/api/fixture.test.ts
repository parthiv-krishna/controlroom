import { describe, expect, it } from 'vitest';
import { loadFixture } from '../../test/fixture';
import type { SessionEventsParams } from '../protocol';
import { Timeline } from '../state/timeline.svelte';
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
    const id = Object.keys(fixture.details)[0];
    const sub = await t.request('session.subscribe', { id, since_seq: 0 });
    expect(sub.detail).toEqual(fixture.details[id]);
  });

  it('records mutations and answers acks or canned results', async () => {
    const t = new FixtureTransport(loadFixture(), { autoplay: false });
    await open(t);
    await expect(t.request('session.cancel', { op_id: 'o', id: 's' })).resolves.toEqual({});
    await expect(t.request('session.prompt', { op_id: 'o', id: 's', content: [], origin: { kind: 'user', device: null }, if_busy: 'queue' })).resolves.toEqual({ outcome: 'sent' });
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

  it('replays a recorded timeline: backlog after the result, then live steps, raw only on request', async () => {
    const fixture = loadFixture();
    const id = Object.keys(fixture.timelines!).find((k) => fixture.timelines![k].live.length)!;
    const recorded = fixture.timelines![id];
    const t = new FixtureTransport(fixture, { autoplay: false });
    await open(t);
    const events: SessionEventsParams[] = [];
    t.subscribe('session.events', (p) => events.push(p));
    const sub = await t.request('session.subscribe', { id, since_seq: 0 });
    expect(sub.head_seq).toBe(recorded.head_seq);
    expect(events).toHaveLength(0);
    await new Promise((r) => setTimeout(r, 0));
    expect(events[0]).toMatchObject({ subscription_id: sub.subscription_id, head_seq: recorded.head_seq, raw: [] });
    expect(events[0].ops).toHaveLength(recorded.items.length);
    expect(t.advance(id, 2)).toBe(2);
    expect(events.slice(1).map((e) => e.head_seq)).toEqual(recorded.live.slice(0, 2).map((s) => s.head_seq));
    // A resubscribe from the head gets nothing twice; one with raw_since gets the raw frames.
    events.length = 0;
    const again = await t.request('session.subscribe', { id, since_seq: recorded.live[1].head_seq, raw_since: 0 });
    await new Promise((r) => setTimeout(r, 0));
    expect(events[0].ops).toEqual([]);
    expect(events[0].raw?.length).toBeGreaterThan(0);
    t.advance(id);
    expect(events.at(-1)).toMatchObject({ subscription_id: again.subscription_id, raw: recorded.live[2].raw });
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
  const sessions = new Set(f.snapshot.sessions.map((s) => s.id));

  it('references only known machines, sessions and projects', () => {
    for (const s of f.snapshot.sessions) for (const w of s.workspaces) expect(machines).toContain(w.machine_id);
    for (const s of f.snapshot.sessions) for (const sh of s.shells) expect(s.workspaces.map((w) => w.id)).toContain(sh.workspace_id);
    for (const key of Object.keys(f.details)) expect(sessions).toContain(key);
    for (const key of Object.keys(f.timelines ?? {})) expect(sessions).toContain(key);
    const projects = new Set(f.snapshot.projects.map((p) => p.id));
    for (const t of f.snapshot.tasks) for (const p of t.project_ids) expect(projects).toContain(p);
    for (const x of f.snapshot.schedules) expect(sessions).toContain(x.session_id);
    for (const o of f.snapshot.overlaps) {
      expect(machines).toContain(o.machine_id);
      for (const id of o.session_ids) expect(sessions).toContain(id);
    }
  });

  it("derives every session's projects from the tasks that link it", () => {
    const order = f.snapshot.projects.map((p) => p.id);
    for (const s of f.snapshot.sessions) {
      const linked = new Set(f.snapshot.tasks.filter((t) => t.links.includes(s.id)).flatMap((t) => t.project_ids));
      expect(s.project_ids).toEqual(order.filter((id) => linked.has(id)));
    }
  });

  it('continues the feed from the cursor', () => {
    const revs = f.script.flatMap((s) => (s.notification === 'state.events' ? s.params.changes.map((c) => c.rev) : []));
    revs.forEach((rev, i) => expect(rev).toBe(f.cursor.rev + i + 1));
  });

  it('contains no real host names', () => {
    for (const m of f.snapshot.machines) expect(m.host).toMatch(/\.example\.com$/);
  });
  it('has consistent recorded timelines', () => {
    for (const [key, t] of Object.entries(f.timelines ?? {})) {
      expect(sessions).toContain(key);
      const ids = t.items.map((it) => it.id);
      expect(new Set(ids).size).toBe(ids.length);
      for (const it of t.items) expect(it.seq_to).toBeLessThanOrEqual(t.head_seq);
      // Live steps move forward, and every append or end targets an item that exists by then.
      const store = new Timeline();
      store.apply(t.items.map((item) => ({ op: 'upsert' as const, item })), 0);
      let head = t.head_seq;
      for (const step of t.live) {
        expect(step.head_seq).toBeGreaterThan(head);
        for (const op of step.ops) {
          if (op.op === 'append_text' || op.op === 'end') expect(store.get(op.id), `${key} ${op.id}`).toBeDefined();
          if (op.op !== 'unknown') expect(op.op === 'upsert' ? op.item.seq_to : op.seq_to).toBeLessThanOrEqual(step.head_seq);
        }
        store.apply(step.ops, head);
        head = step.head_seq;
      }
      const raw = [...t.raw, ...t.live.flatMap((s) => s.raw)].map((r) => r.seq);
      expect(raw).toEqual([...raw].sort((a, b) => a - b));
      expect(new Set(raw).size).toBe(raw.length);
    }
  });

  it('keeps the recordings sanitized', () => {
    const text = JSON.stringify(f.timelines);
    for (const [url] of text.matchAll(/[a-z]+:\/\/[^\s"\\]+/g)) expect(url).toMatch(/^https?:\/\/(127\.0\.0\.1|[a-z.]*example\.com)[:/]/);
    for (const [home] of text.matchAll(/\/home\/[a-z]+/g)) expect(home).toBe('/home/alice');
  });
});
