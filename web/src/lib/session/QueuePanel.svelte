<script lang="ts">
  // The queue: items in send order (an item being handed over shows "sending" and stays until the
  // agent has it), send now, reorder, edit, remove; auto-send; a paused queue resumes.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { contentText, textContent } from '../model/misc';
  import type { MachineSession, QueueItem } from '../protocol';

  let { s, items }: { s: MachineSession; items: QueueItem[] } = $props();
  const app = useApp();
  const online = $derived(app.client.online && !s.stale);

  async function edit(q: QueueItem) {
    const text = await app.dialogs.ask({ title: 'Edit queued message', value: contentText(q.content), multiline: true, ok: 'Save' });
    if (text) await app.client.call('queue.edit', { ...onSession(s), queue_id: q.id, content: textContent(text) });
  }
  const reorder = (q: QueueItem, to_index: number) => app.client.call('queue.reorder', { ...onSession(s), queue_id: q.id, to_index });
</script>

<div class="queue" aria-label="Queue">
  <div class="qh">
    <b>Queue</b> <span class="muted">({s.queue.len})</span>
    {#if s.queue.paused}<span class="paused">paused</span><button class="btn xs primary" disabled={!online} onclick={() => app.client.call('queue.resume', onSession(s))}>Resume</button>{/if}
    <label class="auto" title="Send the next queued message when a turn ends">
      <input type="checkbox" checked={s.queue.auto_send} disabled={!online} onchange={(e) => app.client.call('session.update', { ...onSession(s), auto_send: e.currentTarget.checked })} /> Auto-send
    </label>
  </div>
  {#each items as q, i (q.id)}
    <div class="item" class:sending={q.sending}>
      <span class="i">{i + 1}</span>
      <span class="text" title={contentText(q.content)}>{contentText(q.content)}</span>
      {#if q.origin.kind === 'schedule'}<span class="chip">schedule: {q.origin.name}</span>{/if}
      {#if q.sending}
        <span class="chip sendchip" title="Being handed to the agent; it stays until the agent has it"><span class="spin"></span> sending</span>
      {:else}
        <button class="btn xs" title="Send now, ahead of the queue" disabled={!online} onclick={() => app.client.call('queue.send_now', { ...onSession(s), queue_id: q.id })}>⚡ Send now</button>
        <button class="btn xs ghost" title="Move up" aria-label="Move up" disabled={!online || i === 0} onclick={() => reorder(q, i - 1)}>↑</button>
        <button class="btn xs ghost" title="Move down" aria-label="Move down" disabled={!online || i === items.length - 1} onclick={() => reorder(q, i + 1)}>↓</button>
        <button class="btn xs ghost" title="Edit" aria-label="Edit" disabled={!online} onclick={() => edit(q)}>✎</button>
        <button class="btn xs ghost" title="Remove" aria-label="Remove" disabled={!online} onclick={() => app.client.call('queue.remove', { ...onSession(s), queue_id: q.id })}>✕</button>
      {/if}
    </div>
  {/each}
</div>

<style>
  .queue { margin: 0 auto; max-width: 820px; width: calc(100% - 80px); background: var(--bg3); border-radius: 8px 8px 0 0; padding: 6px 10px; font-size: 13px; }
  .qh { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding-bottom: 4px; }
  .paused { color: var(--bad); font-size: 12px; }
  .auto { margin-left: auto; display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--muted); cursor: pointer; }
  .item { display: flex; gap: 6px; align-items: center; padding: 3px 0; border-top: 1px solid var(--line); }
  .item.sending { color: var(--info); }
  .i { color: var(--muted); width: 16px; }
  .text { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sendchip { color: var(--info); border-color: color-mix(in srgb, var(--info) 40%, var(--line)); }
  @media (max-width: 760px) { .queue { width: auto; margin: 0 12px; } }
</style>
