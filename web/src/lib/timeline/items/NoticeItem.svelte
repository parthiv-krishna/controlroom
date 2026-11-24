<script lang="ts">
  // A notice from the agent or an extension (status lines, notifications). Repeats of the same
  // notice are folded into the first one, with a count. The agent's warning or error is pinned
  // above the composer (`RequestTray`) until dismissed and shows here as a one-line stub that jumps
  // there; once dismissed it shows in full, with the dismissing device in its tooltip.
  import { useApp } from '../../app/app.svelte';
  import { revealPinned } from '../../session/requests';
  import type { ItemOf } from '../../state/timeline.svelte';
  import Icon from '../../ui/Icon.svelte';
  import Markdown from '../../ui/Markdown.svelte';
  import { markdownLine } from '../../ui/markdown';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'notice'> } = $props();
  const ctx = useTimeline();
  const app = useApp();
  const repeats = $derived(ctx.timeline.repeats.get(item.id) ?? 1);
  const ICON = { info: 'ℹ', warning: '⚠', error: '✕', unknown: 'ℹ' } as const;
  const open = $derived(!!item.pinned && !item.dismissed);
  const by = $derived(item.dismissed?.by);
  const byLabel = $derived(by ? (app.client.feed.devices.get(by.device_id)?.label ?? by.label) : null);
</script>

{#if open}
  <button class="stub" class:error={item.level === 'error'} data-stub={item.id} onclick={() => revealPinned(item.id)}>
    <Icon name={item.level === 'error' ? 'cross' : 'warn'} size={12} label={item.level === 'error' ? 'Error' : 'Warning'} /><span class="t">{markdownLine(item.text)}</span><span aria-hidden="true">↓</span>
  </button>
{:else}
  <div class="notice n-{item.level}" title={item.dismissed ? (byLabel ? `Dismissed on ${byLabel}` : 'Dismissed') : undefined}>
    <span class="ic" aria-hidden="true">{ICON[item.level]}</span>
    <Markdown text={item.text} inline />
    {#if repeats > 1}<span class="n" title="Shown {repeats} times">×{repeats}</span>{/if}
  </div>
{/if}

<style>
  .notice { display: flex; gap: 8px; align-items: baseline; font-size: 13px; color: var(--muted); }
  .n-warning .ic { color: var(--warn); }
  .n-error { color: var(--bad); }
  .n { font-size: 11.5px; }
  .stub {
    display: flex; align-items: center; gap: 7px; max-width: 100%; background: none; border: 1px dashed var(--line); border-radius: 8px;
    padding: 3px 10px; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; text-align: left;
  }
  .stub :global(svg) { color: var(--warn); }
  .stub.error :global(svg) { color: var(--bad); }
  .stub:hover { color: var(--fg); }
  .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
