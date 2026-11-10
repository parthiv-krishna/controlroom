<script lang="ts">
  // The session's open requests (its own and its subagents'), oldest first, pinned right above
  // the composer so they don't scroll away with the timeline. The timeline shows a stub for each
  // (`RequestItem`) that jumps here. Capped in height; it scrolls when there are many.
  import { useApp } from '../app/app.svelte';
  import type { FeedSession } from '../protocol';
  import RequestCard from './RequestCard.svelte';
  import { openRequests } from './requests';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const open = $derived(openRequests(s, app.client.sessions.get(s.id)?.timeline.requests ?? new Map()));
</script>

{#if open.length}
  <section class="tray" aria-label="Requests">
    {#if open.length > 1}<div class="count small muted">{open.length} requests</div>{/if}
    {#each open as req (req.id)}<div class="pin" data-pinned={req.id}><RequestCard {req} {s} /></div>{/each}
  </section>
{/if}

<style>
  .tray {
    flex: none; max-height: 40%; overflow-y: auto; display: flex; flex-direction: column; gap: 8px;
    max-width: 900px; width: 100%; margin: 0 auto; padding: 8px 40px 2px;
  }
  .count { position: sticky; top: 0; z-index: 1; background: var(--bg2); padding: 0 2px; }
  .pin { border-radius: 10px; transition: box-shadow 0.4s; }
  .pin:global(.flash) { box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 45%, transparent); }
  @media (max-width: 760px) { .tray { padding: 6px 12px 2px; } }
</style>
