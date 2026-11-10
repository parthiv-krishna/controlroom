<script lang="ts">
  // A request (permission, question, dialog, sandbox, contact, spawn) as it happened. While it is
  // open it is pinned above the composer (`RequestTray`) and shows here as a one-line stub that
  // jumps there; then answered, withdrawn or expired. A subagent's item is answered on the
  // subagent.
  import RequestCard from '../../session/RequestCard.svelte';
  import { revealPinned } from '../../session/requests';
  import { requestOf, type ItemOf } from '../../state/timeline.svelte';
  import Icon from '../../ui/Icon.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'request'> } = $props();
  const ctx = useTimeline();
  const req = $derived(requestOf(item));
</script>

{#if req.state === 'pending'}
  <button class="stub" class:blocking={req.blocking} data-stub={req.id} onclick={() => revealPinned(req.id)}>
    <Icon name={req.blocking ? 'flag' : 'flagOutline'} size={12} /><span class="t">{req.title}</span><span aria-hidden="true">↓</span>
  </button>
{:else}
  <RequestCard {req} s={ctx.s} />
{/if}

<style>
  .stub {
    display: flex; align-items: center; gap: 7px; max-width: 100%; background: none; border: 1px dashed var(--line); border-radius: 8px;
    padding: 3px 10px; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; text-align: left;
  }
  .stub.blocking :global(svg) { color: var(--warn); }
  .stub:hover { color: var(--fg); }
  .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
