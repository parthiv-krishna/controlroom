<script lang="ts">
  // An error the agent or the driver reported.
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'error'> } = $props();
</script>

<div class="err" role="alert"><span aria-hidden="true">✕</span> {item.text}</div>

<style>
  .err { color: var(--bad); font-size: 13.5px; white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
