<script lang="ts">
  // A mailbox message to or from another session, with its outcome.
  import { useApp } from '../../app/app.svelte';
  import type { ItemOf } from '../../state/timeline.svelte';
  import Markdown from '../../ui/Markdown.svelte';
  import { peerSession } from '../peers';

  let { item }: { item: ItemOf<'session_message'> } = $props();
  const app = useApp();
  const target = $derived(peerSession(app, item.peer));
  const OUTCOME = { delivered: 'delivered', queued: 'queued', connection_lost: 'connection lost', denied: 'denied', expired: 'no reply', unknown: '' } as const;
</script>

<div class="sm d-{item.direction} o-{item.outcome ?? 'pending'}">
  <div class="head">
    <span aria-hidden="true">{item.direction === 'sent' ? '→' : '←'}</span>
    {item.direction === 'sent' ? 'to' : 'from'}
    {#if target}<button class="linkish" onclick={() => app.open(target)}>{item.peer.name}</button>{:else}<b>{item.peer.name}</b>{/if}
    {#if item.expects_reply}<span class="chip">expects reply</span>{/if}
    {#if item.late}<span class="chip warn-t" title="Arrived after the ask had resolved">late</span>{/if}
    <span class="out">{item.outcome ? OUTCOME[item.outcome] : 'sending…'}</span>
  </div>
  <div class="text"><Markdown text={item.text} /></div>
</div>

<style>
  .sm { border: 1px solid var(--line); border-radius: 10px; padding: 8px 12px; font-size: 13.5px; }
  .head { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; color: var(--muted); font-size: 12.5px; margin-bottom: 4px; }
  .out { margin-left: auto; }
  .o-denied .out, .o-connection_lost .out { color: var(--bad); }
  .o-delivered .out { color: var(--ok); }
</style>
