<script lang="ts">
  // A message the system wrote for the agent (a schedule's prompt, a job wake, an answer, a notice
  // about workspaces, the sandbox or todo edits, a late result, a subagent's report, another
  // session's message, an auto-continue nudge): one line naming its source. The exact text the
  // agent was told opens on click.
  import { contentText } from '../../model/misc';
  import type { EventOrigin, ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item, origin }: { item: ItemOf<'message'>; origin: EventOrigin } = $props();
  const ctx = useTimeline();
  const open = $derived(ctx.isOpen(item.id, false));
  const ICON: Record<string, string> = { schedule: '◷', auto_continue: '↻', job: '⚙', child: '◆', message: '←', answer: '↳', todo: '☑', tool_late: '↳', workspace: 'ℹ' };
  const icon = $derived(ICON[origin.kind === 'inject' ? origin.origin : origin.kind] ?? 'ℹ');
  const label = $derived(origin.kind === 'schedule' ? `Schedule ${origin.name}` : origin.kind === 'auto_continue' ? 'Auto-continue' : origin.label || 'Notice');
</script>

<div class="ev" data-event={origin.kind === 'inject' ? origin.origin : origin.kind}>
  <button class="head" aria-expanded={open} title={open ? 'Hide what the agent was told' : 'Show what the agent was told'} onclick={() => ctx.toggle(item.id, false)}>
    <span class="ic" aria-hidden="true">{icon}</span><span class="l">{label}</span>
  </button>
  {#if open}<div class="text">{contentText(item.content)}</div>{/if}
</div>

<style>
  .head { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; background: none; border: none; padding: 0; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; text-align: left; }
  .head:hover { color: var(--fg); }
  .ic { flex: none; width: 14px; text-align: center; }
  .l { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .text { white-space: pre-wrap; color: var(--muted); font-size: 13px; border-left: 2px solid var(--line); padding-left: 10px; margin-top: 4px; overflow-wrap: anywhere; }
</style>
