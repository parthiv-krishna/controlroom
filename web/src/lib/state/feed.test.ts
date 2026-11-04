import { describe, expect, it } from 'vitest';
import type { StateChange } from '../protocol';
import { loadFixture } from '../../test/fixture';
import { StateFeed } from './feed.svelte';

const fixture = loadFixture();
const project = fixture.snapshot.projects[0];

describe('StateFeed', () => {
  it('replaces everything with a snapshot', () => {
    const feed = new StateFeed();
    feed.reset(fixture.snapshot, fixture.cursor);
    expect(feed.loaded).toBe(true);
    expect(feed.sessions.size).toBe(fixture.snapshot.sessions.length);
    feed.reset({ ...fixture.snapshot, sessions: fixture.snapshot.sessions.slice(0, 2) }, { epoch: 'E2', rev: 1 });
    expect(feed.sessions.size).toBe(2);
    expect(feed.cursor).toEqual({ epoch: 'E2', rev: 1 });
  });

  it('applies changes in rev order, skipping overlap and reporting gaps', () => {
    const feed = new StateFeed();
    feed.reset(fixture.snapshot, { epoch: 'E', rev: 10 });
    const renamed = { ...project, name: 'Renamed' };
    const changes: StateChange[] = [
      { rev: 9, kind: 'project_removed', project_id: project.id }, // already applied
      { rev: 11, kind: 'project_upserted', project: renamed },
    ];
    expect(feed.apply(changes)).toBe('applied');
    expect(feed.projects.get(project.id)?.name).toBe('Renamed');
    expect(feed.cursor?.rev).toBe(11);
    expect(feed.apply([{ rev: 13, kind: 'project_removed', project_id: project.id }])).toBe('gap');
    expect(feed.projects.has(project.id)).toBe(true);
    expect(feed.cursor?.rev).toBe(11);
  });

  it('removes sessions by id, replaces the overlaps and ignores unknown change kinds', () => {
    const feed = new StateFeed();
    feed.reset(fixture.snapshot, { epoch: 'E', rev: 1 });
    expect(feed.overlaps).toEqual(fixture.snapshot.overlaps);
    const s = fixture.snapshot.sessions[0];
    feed.apply([
      { rev: 2, kind: 'session_removed', session_id: s.id },
      { rev: 3, kind: 'overlaps_changed', overlaps: [] },
      { rev: 4, kind: 'unknown' },
    ]);
    expect(feed.sessions.has(s.id)).toBe(false);
    expect(feed.overlaps).toEqual([]);
    expect(feed.cursor?.rev).toBe(4);
  });

  it('needs a cursor before applying events', () => {
    expect(new StateFeed().apply([])).toBe('gap');
  });
});
