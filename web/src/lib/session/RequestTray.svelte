<script lang="ts">
  // The session's open requests and the agent's open warning and error notices (its own and its
  // subagents'), oldest first, pinned right above the composer so they don't scroll away with the
  // timeline. The timeline shows a stub for each (`RequestItem`, `NoticeItem`) that jumps here. A
  // notice stays until the user dismisses it; opening the session doesn't. Capped in height; it
  // scrolls when there are many.
  import { useApp } from '../app/app.svelte';
  import type { FeedSession } from '../protocol';
  import NoticeCard from './NoticeCard.svelte';
  import RequestCard from './RequestCard.svelte';
  import { openNotices, openRequests, pinnedCount, pinnedItems } from './requests';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const timeline = $derived(app.client.sessions.get(s.id)?.timeline);
  const requests = $derived(openRequests(s, timeline?.requests ?? new Map()));
  const notices = $derived(openNotices(s, timeline?.notices ?? new Map()));
  const pinned = $derived(pinnedItems(requests, notices));
</script>

{#if pinned.length}
  <section class="tray" aria-label="Requests">
    {#if pinned.length > 1}<div class="count small muted">{pinnedCount(requests.length, notices.length)}</div>{/if}
    {#each pinned as p (p.id)}
      <div class="pin" data-pinned={p.id}>
        {#if p.kind === 'request'}<RequestCard req={p.req} {s} />{:else}<NoticeCard notice={p.notice} {s} />{/if}
      </div>
    {/each}
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
