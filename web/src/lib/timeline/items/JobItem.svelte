<script lang="ts">
  // A background job on a workspace's machine, as one line: its name and how it is doing. The
  // command, machine and timing are in the tooltip; a click opens it in the session's Jobs sheet
  // (log file and tail, follow, kill). The wakes it sent the agent are folded in: "told" shows what
  // the agent was told.
  import { useApp } from '../../app/app.svelte';
  import { contentText } from '../../model/misc';
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'job'> } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const j = $derived(item.job);
  const wakes = $derived(ctx.timeline.wakes.get(item.job.id) ?? []);
  const told = $derived(ctx.isOpen(item.id + ':told', false));
  const machine = $derived(j.machine_id ? (app.client.feed.machines.get(j.machine_id)?.name ?? 'a removed machine') : null);
  const ICON = { running: '', succeeded: '✓', failed: '✕', timed_out: '✕', killed: '✕', lost: '?', unknown: '?' } as const;
  const state = $derived.by(() => {
    switch (j.status) {
      case 'running': return 'running';
      case 'succeeded': return 'finished';
      case 'failed': return j.exit !== null ? `failed (exit ${j.exit})` : 'failed';
      case 'timed_out': return 'timed out';
      case 'killed': return j.signal ? `killed (${j.signal})` : 'killed';
      case 'lost': return 'lost';
      default: return 'ended';
    }
  });
</script>

<div class="jw">
<button class="job j-{j.status}" title="{j.command}{machine ? `\non ${machine}` : ''}" onclick={() => (app.view.modal = { kind: 'jobs', session: { id: ctx.s.id }, job: j.id })}>
  {#if j.status === 'running'}<span class="spin"></span>{:else}<span class="ic" aria-hidden="true">{ICON[j.status]}</span>{/if}
  <span>Job <b>{j.name}</b> {state}</span>
</button>
{#if wakes.length}<button class="btn ghost xs" aria-expanded={told} title="What the agent was told" onclick={() => ctx.toggle(item.id + ':told', false)}>told</button>{/if}
{#if told}{#each wakes as w (w.id)}<div class="text">{w.item.kind === 'message' ? contentText(w.item.content) : ''}</div>{/each}{/if}
</div>

<style>
  .jw { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; }
  .text { flex-basis: 100%; white-space: pre-wrap; color: var(--muted); font-size: 13px; border-left: 2px solid var(--line); padding-left: 10px; overflow-wrap: anywhere; }
  .job { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; background: none; border: none; padding: 0; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; text-align: left; }
  .job:hover { color: var(--fg); }
  .ic { width: 14px; text-align: center; flex: none; }
  .j-succeeded .ic { color: var(--ok); }
  .j-failed, .j-timed_out, .j-killed { color: var(--bad); }
</style>
