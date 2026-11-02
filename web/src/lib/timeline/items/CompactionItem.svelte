<script lang="ts">
  // Context compaction: running, done (tokens before → after) or failed.
  import { compact } from '../../model/format';
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'compaction'> } = $props();
  const REASON = { threshold: 'automatic', manual: 'manual', overflow: 'context overflow', unknown: '' } as const;
  const tokens = (n: number | null) => (n === null ? '?' : compact(n));
</script>

<div class="cmp c-{item.phase}">
  {#if item.phase === 'running'}<span class="spin"></span> Compacting context{#if item.tokens_before !== null} · {tokens(item.tokens_before)} tokens{/if}
  {:else if item.phase === 'done'}↓ Context compacted {tokens(item.tokens_before)} → {tokens(item.tokens_after)}
  {:else if item.phase === 'failed'}✕ Compaction failed{#if item.error}: {item.error}{/if}
  {:else}Compaction{/if}
  {#if REASON[item.reason]}<span class="muted small">· {REASON[item.reason]}</span>{/if}
</div>

<style>
  .cmp { display: flex; align-items: center; gap: 6px; font-size: 13px; color: var(--muted); border-top: 1px dashed var(--line); border-bottom: 1px dashed var(--line); padding: 4px 0; flex-wrap: wrap; }
  .c-failed { color: var(--bad); }
</style>
