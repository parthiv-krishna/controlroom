<script lang="ts">
  // A session's workspaces as `machine:path` pills, the primary first, each with its machine's
  // availability ("devbox unreachable since 2h ago": the session keeps its state, only that
  // workspace waits). When another open session uses the same directory, the pill gets a ⚠: its
  // tooltip says who, and a click (or tap) shows links to them.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { availabilityText, dirLabel, isAvailable, overlapsOf, wsDir, wsLabel } from '../model/workspaces';
  import type { FeedSession, Workspace } from '../protocol';
  import { dismissable, fitInView } from '../ui/actions';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const shared = $derived(
    overlapsOf(s, feed.overlaps).map((o) => ({
      where: dirLabel(o.machine_id, o.path, feed.machines),
      others: o.session_ids.filter((id) => id !== s.id).map((id) => feed.sessions.get(id)).filter((x): x is FeedSession => !!x),
    })),
  );
  /** The sessions sharing a workspace's directory, if any. */
  const sharing = (w: Workspace) => shared.find((o) => o.where === wsLabel(w, feed.machines))?.others ?? [];
  let open = $state<string | null>(null);
  let anchors = $state<Record<string, HTMLElement>>({});
  function go(other: FeedSession) {
    open = null;
    app.open(other);
  }
</script>

<span class="wss" aria-label="Workspaces">
  {#each s.workspaces as w (w.id)}
    {@const m = feed.machines.get(w.machine_id)}
    {@const up = isAvailable(w)}
    {@const others = sharing(w)}
    <span class="wrap" class:extra={!w.primary && s.workspaces.length > 1}>
      <span class="ws" class:away={!up} class:primary={w.primary} title={availabilityText(w, feed.machines)} data-workspace={w.id}>
        <span class="dot {w.availability.connectivity.state}"></span><b>{m?.name ?? '?'}</b><span class="muted">:{wsDir(w, feed.machines)}</span>
        {#if !up}<span class="st">{availabilityText(w, feed.machines).slice((m?.name.length ?? 0) + 1)}</span>{/if}
      </span>
      {#if others.length}
        {@const names = others.map(displayName).join(', ')}
        <span class="ov" data-overlap title="{wsLabel(w, feed.machines)} is shared with {names}: edits can collide" bind:this={anchors[w.id]}>
          <button class="warn" aria-label="Shared with {names}" aria-expanded={open === w.id} onclick={() => (open = open === w.id ? null : w.id)}>⚠</button>
          {#if open === w.id}
            <span class="pop" role="dialog" aria-label="Shared directory" use:dismissable={{ close: () => (open = null), anchor: anchors[w.id] }} use:fitInView>
              Shared with {#each others as other, i (other.id)}{i ? ', ' : ''}<button class="linkish" onclick={() => go(other)}>{displayName(other)}</button>{/each}: edits can collide.
            </span>
          {/if}
        </span>
      {/if}
    </span>
  {/each}
  {#if s.workspaces.length > 1}<span class="more" title={s.workspaces.filter((w) => !w.primary).map((w) => wsLabel(w, feed.machines)).join('\n')}>+{s.workspaces.length - 1}</span>{/if}
</span>

<style>
  .wss { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 6px; min-width: 0; }
  .ws { display: inline-flex; align-items: center; font: 12.5px var(--mono); background: var(--bg3); border-radius: 6px; padding: 3px 9px; max-width: 100%; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .ws .dot { margin-right: 6px; }
  .ws.away { background: color-mix(in srgb, var(--warn) 13%, var(--bg2)); }
  .st { font: 11.5px var(--sans, inherit); color: var(--warn); margin-left: 6px; }
  .wrap { display: inline-flex; align-items: center; gap: 2px; min-width: 0; max-width: 100%; }
  .ov { position: relative; display: inline-flex; }
  .warn { background: none; border: none; padding: 0 3px; font: inherit; font-size: 13px; color: var(--warn); cursor: pointer; }
  .pop { position: absolute; z-index: 60; top: calc(100% + 6px); left: 0; width: max-content; max-width: min(360px, 86vw); background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 8px 12px; font-size: 12.5px; }
  .more { display: none; font-size: 12px; color: var(--muted); }
  /* A phone shows the primary workspace and "+N" (the others in its tooltip). */
  @media (max-width: 760px) {
    .extra { display: none; }
    .more { display: inline; }
  }
</style>
