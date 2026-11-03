<script lang="ts">
  // A background job: started, then how it ended, with its log file. "Details" opens it in the
  // session's jobs strip (log tail, follow state, kill).
  import { useApp } from '../../app/app.svelte';
  import { keyOf } from '../../model/projects';
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'job'> } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const j = $derived(item.job);
  const STATUS = { running: 'running', succeeded: 'finished', failed: 'failed', timed_out: 'timed out', killed: 'killed', unknown: 'ended' } as const;
</script>

<div class="job j-{j.status}">
  <div class="head">
    <span aria-hidden="true">⚙</span> job <b>{j.name}</b>
    <span class="st">{#if j.status === 'running'}<span class="spin"></span>{/if} {STATUS[j.status]}{#if j.exit !== null} · exit {j.exit}{/if}{#if j.signal} · {j.signal}{/if}</span>
    {#if j.status !== 'running'}<span class="muted small">{j.lines} lines</span>{/if}
    <button class="linkish small" onclick={() => (app.view.jobOpen[keyOf(ctx.s)] = j.id)}>details</button>
  </div>
  <code class="cmd">{j.command}</code>
  <div class="muted small">log <code>{j.log_path}</code></div>
</div>

<style>
  .job { font-size: 13.5px; display: flex; flex-direction: column; gap: 3px; }
  .head { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .st { display: inline-flex; align-items: center; gap: 4px; color: var(--muted); }
  .j-succeeded .st { color: var(--ok); }
  .j-failed .st, .j-timed_out .st, .j-killed .st { color: var(--bad); }
  .cmd { align-self: flex-start; overflow-wrap: anywhere; }
</style>
