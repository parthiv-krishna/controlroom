<script lang="ts">
  // A change of the session's title, model or thinking level.
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'session_info'> } = $props();
</script>

<div class="info">
  {#if item.title}<span>Title → <b>{item.title}</b></span>{/if}
  {#if item.model}<span>Model → <b>{item.model}</b></span>{/if}
  {#if item.thinking}<span>Thinking → <b>{item.thinking}</b></span>{/if}
</div>

<style>
  .info { display: flex; flex-wrap: wrap; gap: 4px 14px; font-size: 12.5px; color: var(--muted); }
  b { color: var(--fg); font-weight: 600; }
</style>
