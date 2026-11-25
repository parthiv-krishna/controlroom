<script lang="ts">
  // "Shared with <session>, <session>: edits can collide.", each other session a link to it.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import type { FeedSession } from '../protocol';

  let { others, onopen }: { others: FeedSession[]; onopen?: () => void } = $props();
  const app = useApp();
  function go(other: FeedSession) {
    onopen?.();
    app.open(other);
  }
</script>

Shared with {#each others as other, i (other.id)}{i ? ', ' : ''}<button class="linkish" onclick={() => go(other)}>{displayName(other)}</button>{/each}: edits can collide.
