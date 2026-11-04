<script lang="ts">
  // One timeline item: picks the component for its kind and adds what every item has: a `#seq`
  // link to its raw records (shown on hover or focus) and, for a child session's item, its actor.
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
</script>

<div class="item k-{it.kind}" data-item={it.id} data-kind={it.kind}>
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
  <button class="seq" title="Show the source records in the raw view" onclick={() => ctx.showRaw(it)}>{seqLabel}</button>
</div>

<style>
  .item { position: relative; border-radius: 8px; transition: background 0.4s; }
  .item:global(.flash) { background: color-mix(in srgb, var(--warn) 18%, transparent); }
  .seq {
    position: absolute; top: 0; right: 0; opacity: 0; background: var(--bg2); border: 1px solid var(--line); border-radius: 5px;
    font: 11px var(--mono); color: var(--muted); padding: 0 5px; cursor: pointer; z-index: 1;
  }
  .item:hover > .seq, .seq:focus-visible { opacity: 1; }
  .seq:hover { color: var(--fg); }
  .actor { display: inline-block; font-size: 11px; color: var(--muted); border: 1px solid var(--line); border-radius: 999px; padding: 0 7px; margin-bottom: 3px; }
  @media (pointer: coarse) { .seq { opacity: 0.6; } }
</style>
