<script lang="ts">
  // A background job on a workspace's machine: started, then how it ended. "Details" opens it in
  // the session's jobs strip (log file and tail, follow, kill).
  import { useApp } from '../../app/app.svelte';
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'job'> } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const j = $derived(item.job);
  const machines = $derived(app.client.feed.machines);
  const STATUS = { running: 'running', succeeded: 'finished', failed: 'failed', timed_out: 'timed out', killed: 'killed', lost: 'lost', unknown: 'ended' } as const;
</script>

<div class="job j-{j.status}">
  <div class="head">
    <span aria-hidden="true">⚙</span> job <b>{j.name}</b>{#if j.machine_id}<span class="muted small">on {machines.get(j.machine_id)?.name ?? 'a removed machine'}</span>{/if}
    <span class="st">{#if j.status === 'running'}<span class="spin"></span>{/if} {STATUS[j.status]}{#if j.exit !== null} · exit {j.exit}{/if}{#if j.signal} · {j.signal}{/if}</span>
    <button class="linkish small" onclick={() => (app.view.jobOpen[ctx.s.id] = j.id)}>details</button>
  </div>
  <code class="cmd">{j.command}</code>
</div>

<style>
  .job { font-size: 13.5px; display: flex; flex-direction: column; gap: 3px; }
  .head { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .st { display: inline-flex; align-items: center; gap: 4px; color: var(--muted); }
  .j-succeeded .st { color: var(--ok); }
  .j-failed .st, .j-timed_out .st, .j-killed .st { color: var(--bad); }
  .cmd { align-self: flex-start; overflow-wrap: anywhere; }
</style>
