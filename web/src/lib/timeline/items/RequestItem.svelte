<script lang="ts">
  // A request (permission, question, dialog, sandbox, contact, spawn) as it happened: answerable
  // inline while pending, then answered, withdrawn or expired. A subagent's item is answered on
  // the subagent.
  import RequestCard from '../../session/RequestCard.svelte';
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'request'> } = $props();
  const ctx = useTimeline();
  const req = $derived(item.actor === 'root' || item.request.origin_child ? item.request : { ...item.request, origin_child: item.actor });
</script>

<RequestCard {req} s={ctx.s} />
