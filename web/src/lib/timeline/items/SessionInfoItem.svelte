<script lang="ts">
  // A change of the session's title, model, thinking level or sandbox level, as a thin divider.
  // Only changed values are shown: the first ones are what the header already shows (the timeline
  // folds the rest away). A sandbox level is always a change (the user set it); off is red.
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'session_info'> } = $props();
  const ctx = useTimeline();
  const changed = $derived(ctx.timeline.infoChanges.get(item.id) ?? {});
</script>

<div class="info" role="separator">
  <span>
    {#if changed.title}title <b>{changed.title}</b>{/if}
    {#if changed.model}model <b>{changed.model}</b>{/if}
    {#if changed.thinking}thinking <b>{changed.thinking}</b>{/if}
    {#if changed.sandbox}<span data-sandbox={changed.sandbox}>sandbox → <b class:off={changed.sandbox === 'off'}>{changed.sandbox}</b></span>{/if}
  </span>
</div>

<style>
  .info { display: flex; align-items: center; gap: 10px; font-size: 11.5px; color: var(--muted); }
  .info::before, .info::after { content: ''; flex: 1; border-top: 1px solid var(--line); }
  span { display: inline-flex; gap: 12px; }
  span span { gap: 0.3em; }
  b { color: var(--fg); font-weight: 500; }
  b.off { color: var(--bad); font-weight: 650; }
</style>
