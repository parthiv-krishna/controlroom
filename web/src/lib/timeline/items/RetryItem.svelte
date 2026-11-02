<script lang="ts">
  // An automatic retry after a provider error.
  import { duration } from '../../model/status';
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'retry'> } = $props();
  const PHASE = { waiting: 'waiting', retrying: 'retrying', succeeded: 'succeeded', gave_up: 'gave up', unknown: '' } as const;
</script>

<div class="retry r-{item.phase}">
  {#if item.phase === 'retrying'}<span class="spin"></span>{:else}<span aria-hidden="true">↻</span>{/if}
  Retry {item.attempt}{#if item.max_attempts !== null}/{item.max_attempts}{/if}
  · {PHASE[item.phase]}{#if item.phase === 'waiting' && item.delay_ms !== null} {duration(item.delay_ms)}{/if}
  {#if item.error}<span class="error">· {item.error}</span>{/if}
</div>

<style>
  .retry { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; font-size: 13px; color: var(--muted); }
  .r-gave_up { color: var(--bad); }
  .r-succeeded { color: var(--ok); }
  .error { overflow-wrap: anywhere; }
</style>
