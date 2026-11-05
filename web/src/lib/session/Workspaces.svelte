<script lang="ts">
  // A session's workspaces as `machine:path` pills, the primary first, each with its machine's
  // availability ("devbox unreachable since 2h ago": the session keeps its state, only that
  // workspace waits), and a warning when another open session uses the same directory.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { availabilityText, dirLabel, isAvailable, overlapsOf, wsDir } from '../model/workspaces';
  import type { FeedSession } from '../protocol';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const shared = $derived(
    overlapsOf(s, feed.overlaps).map((o) => ({
      where: dirLabel(o.machine_id, o.path, feed.machines),
      others: o.session_ids.filter((id) => id !== s.id).map((id) => feed.sessions.get(id)).filter((x): x is FeedSession => !!x),
    })),
  );
</script>

<span class="wss" aria-label="Workspaces">
  {#each s.workspaces as w (w.id)}
    {@const m = feed.machines.get(w.machine_id)}
    {@const up = isAvailable(w)}
    <span class="ws" class:away={!up} class:primary={w.primary} title={availabilityText(w, feed.machines)} data-workspace={w.id}>
      <span class="dot {w.availability.connectivity.state}"></span><b>{m?.name ?? '?'}</b><span class="muted">:{wsDir(w, feed.machines)}</span>
      {#if !up}<span class="st">{availabilityText(w, feed.machines).slice((m?.name.length ?? 0) + 1)}</span>{/if}
    </span>
  {/each}
  {#each shared as o (o.where)}
    <span class="overlap" role="note" data-overlap title="{o.where}: edits can collide">
      ⚠ shared with
      {#each o.others as other, i (other.id)}{i ? ', ' : ' '}<button class="linkish" onclick={() => app.open(other)}>{displayName(other)}</button>{/each}
    </span>
  {/each}
</span>

<style>
  .wss { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 6px; min-width: 0; }
  .ws { display: inline-flex; align-items: center; font: 12.5px var(--mono); background: var(--bg3); border-radius: 6px; padding: 3px 9px; max-width: 100%; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .ws .dot { margin-right: 6px; }
  .ws.away { background: color-mix(in srgb, var(--warn) 13%, var(--bg2)); }
  .st { font: 11.5px var(--sans, inherit); color: var(--warn); margin-left: 6px; }
  .overlap { font-size: 12px; color: var(--warn); }
</style>
