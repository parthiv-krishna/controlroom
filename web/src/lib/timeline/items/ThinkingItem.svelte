<script lang="ts">
  // The agent's reasoning, collapsed by default.
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'thinking'> } = $props();
  const ctx = useTimeline();
  const open = $derived(ctx.isOpen(item.id, false));
</script>

<div class="thinking">
  <button class="head" aria-expanded={open} onclick={() => ctx.toggle(item.id, false)}>
    {open ? '▾' : '▸'} Thinking{#if item.streaming}<span class="spin"></span>{/if}
  </button>
  {#if open}<div class="text">{item.text}</div>{/if}
</div>

<style>
  .head { background: none; border: none; padding: 0; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; display: inline-flex; align-items: center; gap: 6px; }
  .text { white-space: pre-wrap; color: var(--muted); font-size: 13.5px; border-left: 2px solid var(--line); padding-left: 10px; margin-top: 4px; overflow-wrap: anywhere; }
</style>
