<script lang="ts">
  // A notice from the agent or an extension (status lines, notifications).
  import type { ItemOf } from '../../state/timeline.svelte';
  import Markdown from '../../ui/Markdown.svelte';

  let { item }: { item: ItemOf<'notice'> } = $props();
  const ICON = { info: 'ℹ', warning: '⚠', error: '✕', unknown: 'ℹ' } as const;
</script>

<div class="notice n-{item.level}">
  <span class="ic" aria-hidden="true">{ICON[item.level]}</span>
  <Markdown text={item.text} inline />
</div>

<style>
  .notice { display: flex; gap: 8px; align-items: baseline; font-size: 13px; color: var(--muted); }
  .n-warning .ic { color: var(--warn); }
  .n-error { color: var(--bad); }
</style>
