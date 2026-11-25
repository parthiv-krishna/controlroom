<script lang="ts">
  // One workspace in the workspaces popover: `machine:path`, its status in words, "primary" on the
  // primary, the overlap warning with links to the other sessions, and Remove (not the primary)
  // behind a one-line confirm. A refusal (a command still running there) shows on the row.
  import { newOpId, useApp } from '../app/app.svelte';
  import { RpcError, describeError } from '../api/errors';
  import { availabilityState, isAvailable, wsDir, wsLabel } from '../model/workspaces';
  import type { FeedSession, Workspace } from '../protocol';
  import SharedWith from './SharedWith.svelte';

  let { s, w, others, onopen }: { s: FeedSession; w: Workspace; others: FeedSession[]; onopen: () => void } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const label = $derived(wsLabel(w, feed.machines));
  let confirming = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function remove() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await app.client.request('session.workspace_remove', { op_id: newOpId(), id: s.id, workspace_id: w.id });
      confirming = false;
    } catch (e) {
      error = e instanceof RpcError && e.data.kind === 'busy' ? 'A command or job is still running there' : describeError(e);
    } finally {
      busy = false;
    }
  }
</script>

<li class="row" class:away={!isAvailable(w)} data-workspace={w.id} aria-label={label}>
  <div class="line">
    <span class="dot {w.availability.connectivity.state}"></span>
    <span class="where"><b>{feed.machines.get(w.machine_id)?.name ?? '?'}</b><span class="muted">:{wsDir(w, feed.machines)}</span></span>
    {#if w.primary}<span class="prim" title="Relative paths and new shells default to the primary workspace">primary</span>{/if}
    <span class="sp"></span>
    {#if !w.primary && !confirming}
      <button class="btn xs ghost" aria-label="Remove {label}" disabled={!app.client.online} onclick={() => ((confirming = true), (error = null))}>Remove</button>
    {/if}
  </div>
  <div class="state">{availabilityState(w)}</div>
  {#if others.length}<div class="ov" data-overlap>⚠ <SharedWith {others} {onopen} /></div>{/if}
  {#if confirming}
    <div class="confirm" role="group" aria-label="Confirm removal">
      <span class="q">Remove {label}? Its shells end.</span>
      <span class="cb">
        <button class="btn xs danger" disabled={busy || !app.client.online} onclick={remove}>Remove</button>
        <button class="btn xs ghost" onclick={() => (confirming = false)}>Cancel</button>
      </span>
    </div>
  {/if}
  {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
</li>

<style>
  .row { display: flex; flex-direction: column; gap: 2px; padding: 7px 4px; border-top: 1px solid var(--line); }
  .row:first-child { border-top: none; }
  .line { display: flex; align-items: center; gap: 7px; min-width: 0; }
  .where { font: 12.5px var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .prim { flex: none; font-size: 11px; color: var(--muted); border: 1px solid var(--line); border-radius: 999px; padding: 0 6px; }
  .sp { flex: 1; }
  .state { font-size: 12px; color: var(--muted); padding-left: 15px; }
  .away .state { color: var(--warn); }
  .ov { font-size: 12px; color: var(--warn); padding-left: 15px; }
  .ov :global(.linkish) { color: var(--fg); }
  .confirm { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 4px 0 0 15px; font-size: 12.5px; }
  .q { margin-right: auto; }
  .cb { display: inline-flex; gap: 6px; margin-left: auto; }
  .bad-t { padding-left: 15px; }
  @media (max-width: 760px) { .row { padding: 10px 4px; } .row :global(.btn) { min-height: 36px; } }
</style>
