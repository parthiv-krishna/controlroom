<script lang="ts">
  // A subagent (child session): its title, state, outcome and result (its usage is in its own
  // header). A child on this controller's machines can be opened, and its live tail followed here
  // (a second subscription).
  import { useApp } from '../../app/app.svelte';
  import type { ItemOf } from '../../state/timeline.svelte';
  import Markdown from '../../ui/Markdown.svelte';
  import { useTimeline } from '../context';
  import { peerSession } from '../peers';
  import ChildTail from './ChildTail.svelte';

  let { item }: { item: ItemOf<'subagent'> } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const target = $derived(peerSession(app, item.child));
  const live = $derived(ctx.isOpen(item.id, false));
</script>

<div class="sub st-{item.state}">
  <div class="head">
    <span aria-hidden="true">◆</span>
    {#if target}<button class="linkish" onclick={() => app.open(target)}>{item.title}</button>{:else}<b>{item.title}</b>{/if}
    <span class="muted small">{item.agent}</span>
    <span class="state">{#if item.state === 'working' || item.state === 'starting'}<span class="spin"></span>{/if} {item.state.replace('_', ' ')}{#if item.outcome} · {item.outcome}{/if}</span>
    {#if target}<button class="btn ghost xs" aria-expanded={live} onclick={() => ctx.toggle(item.id, false)}>{live ? 'hide live' : 'live'}</button>{/if}
  </div>
  {#if item.result}<div class="result"><Markdown text={item.result} /></div>{/if}
  {#if live && target}<ChildTail child={target} />{/if}
</div>

<style>
  .sub { border-left: 3px solid var(--info); padding: 2px 0 2px 10px; font-size: 13.5px; }
  .head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .state { display: inline-flex; align-items: center; gap: 4px; color: var(--muted); font-size: 12.5px; }
  .result { margin-top: 4px; }
</style>
