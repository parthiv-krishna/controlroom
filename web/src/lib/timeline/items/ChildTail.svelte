<script lang="ts">
  // The last few items of a child session, live: holds the child's feed while shown (one more
  // concurrent subscription), and releases it when hidden.
  import { untrack } from 'svelte';
  import { useApp } from '../../app/app.svelte';
  import type { FeedSession } from '../../protocol';
  import { Timeline } from '../../state/timeline.svelte';
  import { provideTimeline, useTimeline } from '../context';
  import ItemView from '../ItemView.svelte';

  let { child }: { child: FeedSession } = $props();
  const app = useApp();
  const parent = useTimeline();
  const TAIL = 4;

  $effect(() => {
    const { id } = child;
    const feed = untrack(() => app.client.sessions.acquire(id, app.client.online));
    return () => untrack(() => app.client.sessions.release(feed));
  });
  const timeline = $derived(app.client.sessions.get(child.id)?.timeline ?? new Timeline());
  const tail = $derived(timeline.rows.slice(-TAIL));

  provideTimeline({
    get s() {
      return child;
    },
    get timeline() {
      return timeline;
    },
    isOpen: (id, fallback) => parent.isOpen(`${child.id}/${id}`, fallback),
    toggle: (id, fallback) => parent.toggle(`${child.id}/${id}`, fallback),
    showRaw: () => app.open(child),
    get active() {
      const prefix = `${child.id}/`;
      return parent.active?.startsWith(prefix) ? parent.active.slice(prefix.length) : null;
    },
    activate: (id) => parent.activate(id && `${child.id}/${id}`),
  });
</script>

<div class="tail" aria-label="Subagent activity">
  {#each tail as cell (cell.id)}<ItemView {cell} depth={1} />{:else}<span class="muted small">No activity yet</span>{/each}
</div>

<style>
  .tail { display: flex; flex-direction: column; gap: 8px; margin-top: 6px; padding: 8px 10px; border-radius: 8px; background: var(--bg3); font-size: 13px; }
</style>
