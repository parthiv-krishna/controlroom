<script lang="ts">
  // Context compaction: running, done (tokens before → after) or failed.
  import { compact } from '../../model/format';
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'compaction'> } = $props();
  const tokens = (n: number | null) => (n === null ? '?' : compact(n));
</script>

<div class="cmp c-{item.phase}" title={item.reason === 'unknown' ? undefined : `Reason: ${item.reason}`}>
  {#if item.phase === 'running'}<span class="spin"></span> Compacting context
  {:else if item.phase === 'done'}↓ Context compacted {tokens(item.tokens_before)} → {tokens(item.tokens_after)}
  {:else if item.phase === 'failed'}✕ {item.error?.startsWith('Compaction failed') ? item.error : `Compaction failed${item.error ? `: ${item.error}` : ''}`}
  {:else}Compaction{/if}
</div>

<style>
  .cmp { display: flex; align-items: center; gap: 6px; font-size: 13px; color: var(--muted); border-top: 1px dashed var(--line); border-bottom: 1px dashed var(--line); padding: 4px 0; flex-wrap: wrap; }
  .c-failed { color: var(--bad); }
</style>
