<script lang="ts">
  // A tool call: title (remote files as `machine:path`), status, exit and duration; expanded, its
  // input and output. Nested calls (`parent_tool_call_id`) are shown inside their parent.
  import { elapsed } from '../../model/format';
  import type { ItemOf } from '../../state/timeline.svelte';
  import MachinePaths from '../../ui/MachinePaths.svelte';
  import { useTimeline } from '../context';
  import ItemView from '../ItemView.svelte';
  import ToolOutputView from './ToolOutputView.svelte';

  let { item, depth = 0 }: { item: ItemOf<'tool_call'>; depth?: number } = $props();
  const ctx = useTimeline();
  const running = $derived(item.status === 'running' || item.status === 'pending');
  const open = $derived(ctx.isOpen(item.id, running));
  const children = $derived(ctx.timeline.children.get(item.tool_call_id) ?? []);
  const status = $derived(
    [item.status, item.exit !== null && item.exit !== 0 ? `exit ${item.exit}` : null, item.duration_ms !== null ? elapsed(item.duration_ms) : null].filter(Boolean).join(' · '),
  );
  const GLYPH = { read: '◰', edit: '✎', execute: '$', search: '⌕', fetch: '⇣', other: '◇', unknown: '◇' } as const;
</script>

<div class="tool s-{item.status}" class:nested={depth > 0}>
  <button class="head" aria-expanded={open} onclick={() => ctx.toggle(item.id, running)}>
    <span class="glyph" aria-hidden="true">{GLYPH[item.tool_kind]}</span>
    <span class="title"><MachinePaths text={item.title || item.name} /></span>
    {#if children.length}<span class="kids">{children.length} sub-call{children.length === 1 ? '' : 's'}</span>{/if}
    <span class="st">{#if running}<span class="spin"></span>{/if}{status}</span>
  </button>
  {#if open}
    <div class="detail">
      {#if item.input}<pre class="input">{item.input}</pre>{/if}
      {#if item.output}<ToolOutputView output={item.output} />{/if}
    </div>
  {/if}
  {#if children.length && (open || running || depth === 0)}
    <div class="children">
      {#each children as cell (cell.id)}<ItemView {cell} depth={depth + 1} />{/each}
    </div>
  {/if}
</div>

<style>
  .tool { border: 1px solid var(--line); border-radius: 9px; padding: 5px 10px; font-size: 13px; }
  .nested { border-style: dashed; }
  .head { width: 100%; display: flex; align-items: center; gap: 8px; background: none; border: none; padding: 0; font: inherit; color: inherit; cursor: pointer; text-align: left; min-width: 0; }
  .glyph { font-family: var(--mono); color: var(--muted); width: 14px; text-align: center; flex: none; }
  .title { font-family: var(--mono); font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .kids { font-size: 11px; color: var(--muted); flex: none; }
  .st { margin-left: auto; display: inline-flex; align-items: center; gap: 5px; color: var(--muted); font-size: 12px; flex: none; padding-right: 36px; }
  .s-completed .st { color: var(--ok); }
  .s-failed .st { color: var(--bad); }
  .s-cancelled .st { color: var(--warn); }
  .detail { display: flex; flex-direction: column; gap: 6px; margin-top: 6px; }
  .input { color: var(--muted); font-size: 12px; max-height: 160px; overflow: auto; }
  .children { display: flex; flex-direction: column; gap: 6px; margin: 6px 0 2px 12px; }
</style>
