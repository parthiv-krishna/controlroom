<script lang="ts">
  // One timeline item: picks the component for its kind and adds what every item has: a details
  // button in the right gutter (its time and `#seq` range; opens its raw records), shown on hover
  // or focus, or on a touch screen once the item is tapped; and, for a child session's item, its
  // actor.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import type { ItemCell } from '../state/timeline.svelte';
  import { useTimeline } from './context';
  import CompactionItem from './items/CompactionItem.svelte';
  import ErrorItem from './items/ErrorItem.svelte';
  import JobItem from './items/JobItem.svelte';
  import MessageItem from './items/MessageItem.svelte';
  import NoticeItem from './items/NoticeItem.svelte';
  import RequestItem from './items/RequestItem.svelte';
  import RetryItem from './items/RetryItem.svelte';
  import RunItem from './items/RunItem.svelte';
  import SessionInfoItem from './items/SessionInfoItem.svelte';
  import SessionMessageItem from './items/SessionMessageItem.svelte';
  import SubagentItem from './items/SubagentItem.svelte';
  import ThinkingItem from './items/ThinkingItem.svelte';
  import TodoItem from './items/TodoItem.svelte';
  import ToolCallItem from './items/ToolCallItem.svelte';
  import UnknownItem from './items/UnknownItem.svelte';
  import UsageItem from './items/UsageItem.svelte';

  let { cell, depth = 0 }: { cell: ItemCell; depth?: number } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const it = $derived(cell.item);
  const actor = $derived.by(() => {
    if (it.actor === 'root') return null;
    const child = app.client.feed.sessions.get(it.actor);
    return child ? displayName(child) : 'subagent';
  });
  const seqLabel = $derived(it.seq_from === it.seq_to ? `#${it.seq_from}` : `#${it.seq_from}–${it.seq_to}`);
  const details = $derived(`Raw records ${seqLabel} · ${new Date(it.ts).toLocaleString()}`);

  /** A tap outside the item's own controls selects it (touch screens show its details button then). */
  function select(e: MouseEvent) {
    if ((e.target as Element).closest('button, a, input, textarea, select, label, summary, [role="button"]')) return;
    e.stopPropagation();
    ctx.activate(ctx.active === it.id ? null : it.id);
  }
</script>

<!-- Keyboard users reach the details button with Tab. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="item k-{it.kind}" class:active={ctx.active === it.id} data-item={it.id} data-kind={it.kind} onclick={select}>
  {#if actor}<span class="actor" title="From a subagent">◆ {actor}</span>{/if}
  {#if it.kind === 'run'}<RunItem item={it} />
  {:else if it.kind === 'message'}<MessageItem item={it} />
  {:else if it.kind === 'thinking'}<ThinkingItem item={it} />
  {:else if it.kind === 'tool_call'}<ToolCallItem item={it} {depth} />
  {:else if it.kind === 'request'}<RequestItem item={it} />
  {:else if it.kind === 'usage'}<UsageItem item={it} />
  {:else if it.kind === 'compaction'}<CompactionItem item={it} />
  {:else if it.kind === 'retry'}<RetryItem item={it} />
  {:else if it.kind === 'notice'}<NoticeItem item={it} />
  {:else if it.kind === 'error'}<ErrorItem item={it} />
  {:else if it.kind === 'session_info'}<SessionInfoItem item={it} />
  {:else if it.kind === 'todo'}<TodoItem item={it} />
  {:else if it.kind === 'subagent'}<SubagentItem item={it} />
  {:else if it.kind === 'job'}<JobItem item={it} />
  {:else if it.kind === 'session_message'}<SessionMessageItem item={it} />
  {:else}<UnknownItem />{/if}
  <button class="more" title={details} aria-label={details} onclick={() => ctx.showRaw(it)}>⋯</button>
</div>

<style>
  .item { position: relative; border-radius: 8px; transition: background 0.4s; }
  .item:global(.flash) { background: color-mix(in srgb, var(--warn) 18%, transparent); }
  /* In the right gutter (the timeline's side padding), never over the item's text. */
  .more {
    position: absolute; top: 0; left: 100%; margin-left: 2px; width: 14px; opacity: 0; background: none; border: none; padding: 0;
    font-size: 13px; line-height: 1.4; color: var(--muted); cursor: pointer; z-index: 1;
  }
  /* The innermost hovered item only (a nested tool call, not also its parent). */
  .item:hover:not(:has(.item:hover)) > .more, .more:focus-visible, .item.active > .more { opacity: 1; }
  .more:hover { color: var(--fg); }
  .actor { display: inline-block; font-size: 11px; color: var(--muted); border: 1px solid var(--line); border-radius: 999px; padding: 0 7px; margin-bottom: 3px; }
  /* Touch screens: a tap selects the item instead (a tap also leaves a sticky :hover behind). */
  @media (hover: none) { .item:not(.active):hover > .more:not(:focus-visible) { opacity: 0; } }
</style>
