<script lang="ts">
  // Token usage of the session (or a turn, though the timeline hides those: the header has the
  // totals), with cost and context fill when known.
  import { compact } from '../../model/format';
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'usage'> } = $props();
  const t = $derived(item.tokens);
  const cached = $derived(t.cache_read + t.cache_write);
</script>

<div class="usage" title="Token usage{item.scope === 'session' ? ' (session total)' : ''}">
  {item.scope === 'session' ? 'session' : 'turn'} · {compact(t.input)} in · {compact(t.output)} out{#if cached} · {compact(cached)} cache{/if}
  {#if item.cost !== null} · ${item.cost.toFixed(4)}{/if}
  {#if item.context} · context {compact(item.context.used)}{#if item.context.size}/{compact(item.context.size)}{/if}{/if}
</div>

<style>
  .usage { font: 11.5px var(--mono); color: var(--muted); text-align: right; }
</style>
