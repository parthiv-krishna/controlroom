<script lang="ts">
  // A session's only workspace as a `machine:path` pill with its machine's availability ("devbox
  // unreachable since 2h ago": the session keeps its state, only the workspace waits). When
  // another open session uses the same directory, a ⚠ follows: its tooltip says who, and a click
  // (or tap) shows links to them.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { availabilityState, availabilityText, isAvailable, wsDir, wsLabel } from '../model/workspaces';
  import type { FeedSession, Workspace } from '../protocol';
  import { dismissable, fitInView } from '../ui/actions';
  import SharedWith from './SharedWith.svelte';

  let { w, others }: { w: Workspace; others: FeedSession[] } = $props();
  const feed = useApp().client.feed;
  const up = $derived(isAvailable(w));
  let open = $state(false);
  let anchor: HTMLElement | undefined = $state();
</script>

<span class="ws" class:away={!up} title={availabilityText(w, feed.machines)} data-workspace={w.id}>
  <span class="dot {w.availability.connectivity.state}"></span><b>{feed.machines.get(w.machine_id)?.name ?? '?'}</b><span class="muted">:{wsDir(w, feed.machines)}</span>
  {#if !up}<span class="st">{availabilityState(w)}</span>{/if}
</span>
{#if others.length}
  {@const names = others.map(displayName).join(', ')}
  <span class="ov" data-overlap title="{wsLabel(w, feed.machines)} is shared with {names}: edits can collide" bind:this={anchor}>
    <button class="warn" aria-label="Shared with {names}" aria-expanded={open} onclick={() => (open = !open)}>⚠</button>
    {#if open}
      <span class="pop" role="dialog" aria-label="Shared directory" use:dismissable={{ close: () => (open = false), anchor }} use:fitInView>
        <SharedWith {others} onopen={() => (open = false)} />
      </span>
    {/if}
  </span>
{/if}

<style>
  .ws { display: inline-flex; align-items: center; font: 12.5px var(--mono); background: var(--bg3); border-radius: 6px; padding: 3px 9px; max-width: 100%; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .ws .dot { margin-right: 6px; }
  .ws.away { background: color-mix(in srgb, var(--warn) 13%, var(--bg2)); }
  .st { font: 11.5px var(--sans, inherit); color: var(--warn); margin-left: 6px; }
  .ov { position: relative; display: inline-flex; }
  .warn { background: none; border: none; padding: 0 3px; font: inherit; font-size: 13px; color: var(--warn); cursor: pointer; }
  .pop { position: absolute; z-index: 60; top: calc(100% + 6px); left: 0; width: max-content; max-width: min(360px, 86vw); background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 8px 12px; font-size: 12.5px; }
</style>
