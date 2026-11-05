<script lang="ts">
  // A notice from the agent or an extension (status lines, notifications). Repeats of the same
  // notice are folded into the first one, with a count.
  import type { ItemOf } from '../../state/timeline.svelte';
  import Markdown from '../../ui/Markdown.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'notice'> } = $props();
  const ctx = useTimeline();
  const repeats = $derived(ctx.timeline.repeats.get(item.id) ?? 1);
  const ICON = { info: 'ℹ', warning: '⚠', error: '✕', unknown: 'ℹ' } as const;
</script>

<div class="notice n-{item.level}">
  <span class="ic" aria-hidden="true">{ICON[item.level]}</span>
  <Markdown text={item.text} inline />
  {#if repeats > 1}<span class="n" title="Shown {repeats} times">×{repeats}</span>{/if}
</div>

<style>
  .notice { display: flex; gap: 8px; align-items: baseline; font-size: 13px; color: var(--muted); }
  .n-warning .ic { color: var(--warn); }
  .n-error { color: var(--bad); }
  .n { font-size: 11.5px; }
</style>
