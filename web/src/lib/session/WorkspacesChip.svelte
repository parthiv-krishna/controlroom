<script lang="ts">
  // Several workspaces as one chip: "3 workspaces", a status dot per workspace (primary first) and
  // a ⚠ when one of them is shared with another open session. A click (or tap) opens the list
  // (a popover; a bottom sheet on phones), where each workspace shows its status, the overlap
  // with links to the other sessions, and Remove.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { availabilityText, isAvailable, wsLabel } from '../model/workspaces';
  import type { FeedSession, Workspace } from '../protocol';
  import { dismissable, fitInView } from '../ui/actions';
  import WorkspaceRow from './WorkspaceRow.svelte';

  let { s, sharing }: { s: FeedSession; sharing: (w: Workspace) => FeedSession[] } = $props();
  const feed = useApp().client.feed;
  let open = $state(false);
  let anchor: HTMLElement | undefined = $state();

  const n = $derived(s.workspaces.length);
  const away = $derived(s.workspaces.filter((w) => !isAvailable(w)).length);
  const shared = $derived(s.workspaces.filter((w) => sharing(w).length));
  const aria = $derived(
    [`${n} workspaces`, away ? `${away} unavailable` : '', shared.length ? `${shared.length} shared with another session` : ''].filter(Boolean).join(', '),
  );
  const title = $derived(
    [
      ...s.workspaces.map((w) => `${wsLabel(w, feed.machines)}: ${availabilityText(w, feed.machines)}`),
      ...shared.map((w) => `⚠ ${wsLabel(w, feed.machines)} is shared with ${sharing(w).map(displayName).join(', ')}`),
    ].join('\n'),
  );
</script>

<span class="wrap" bind:this={anchor}>
  <button class="chip" class:away={away > 0} aria-label={aria} {title} aria-haspopup="dialog" aria-expanded={open} onclick={() => (open = !open)}>
    {n} workspaces
    <span class="dots" aria-hidden="true">{#each s.workspaces as w (w.id)}<span class="dot {w.availability.connectivity.state}"></span>{/each}</span>
    {#if shared.length}<span class="warn" aria-hidden="true">⚠</span>{/if}
  </button>
  {#if open}
    <div class="pop" role="dialog" aria-label="Session workspaces" use:dismissable={{ close: () => (open = false), anchor }} use:fitInView>
      <ul>
        {#each s.workspaces as w (w.id)}<WorkspaceRow {s} {w} others={sharing(w)} onopen={() => (open = false)} />{/each}
      </ul>
    </div>
  {/if}
</span>

<style>
  .wrap { position: relative; display: inline-flex; min-width: 0; }
  .chip { display: inline-flex; align-items: center; gap: 7px; font: inherit; font-size: 12.5px; color: var(--fg); background: var(--bg3); border: none; border-radius: 6px; padding: 3px 9px; cursor: pointer; white-space: nowrap; }
  .chip:hover, .chip[aria-expanded='true'] { background: color-mix(in srgb, var(--accent) 15%, var(--bg3)); }
  .chip.away { background: color-mix(in srgb, var(--warn) 13%, var(--bg2)); }
  .dots { display: inline-flex; gap: 3px; }
  .dots .dot { width: 7px; height: 7px; }
  .warn { color: var(--warn); font-size: 13px; }
  .pop { position: absolute; z-index: 60; top: calc(100% + 6px); left: 0; width: min(400px, 92vw); background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 4px 10px; }
  ul { list-style: none; margin: 0; padding: 0; }
  @media (max-width: 760px) {
    .pop { position: fixed; left: 0; right: 0; bottom: 0; top: auto; width: auto; max-height: 70dvh; overflow: auto; border-radius: 16px 16px 0 0; padding: 8px 14px calc(env(safe-area-inset-bottom) + 10px); z-index: 90; }
  }
</style>
