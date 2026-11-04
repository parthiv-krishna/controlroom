<script lang="ts">
  // Banners above the timeline: each machine of the session's workspaces that can't be reached
  // (unreachable, connecting, action required, outdated; the session keeps running, only tool
  // calls there wait), asleep, interrupted, failed, archived, and a send-now waiting for a safe
  // point.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { ago, connLabel, duration } from '../model/status';
  import { unavailable } from '../model/workspaces';
  import type { FeedSession, Machine } from '../protocol';
  import MachineFix from '../settings/MachineFix.svelte';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  /** The machines of unavailable workspaces, each once. */
  const away = $derived([...new Set(unavailable(s).map((w) => w.machine_id))].map((id) => app.client.feed.machines.get(id)).filter((m): m is Machine => !!m));
  const online = $derived(app.client.online);
  const startFresh = () => (app.view.modal = { kind: 'start-fresh', session: { id: s.id } });
  const resume = () => app.client.call('session.resume', onSession(s));
</script>

{#each away as machine (machine.id)}
  {#if machine.connectivity.state === 'unreachable'}
    <div class="banner warn" data-machine-banner={machine.name}>⚠ <b>{machine.name}</b> unreachable · last seen {ago(machine.last_seen)} · tool calls there wait up to {duration(machine.grace_s * 1000)} <button class="btn sm" disabled={!app.client.online} onclick={() => app.client.call('machines.reconnect', { machine_id: machine.id })}>Retry now</button></div>
  {:else if machine.connectivity.state === 'action_required'}
    <div class="banner act"><MachineFix m={machine} compact /></div>
  {:else if machine.connectivity.state === 'incompatible'}
    <div class="banner warn">⚠ {machine.name}: daemon {machine.version ?? ''} is outdated <button class="btn sm primary" disabled={!app.client.online} onclick={() => app.client.call('machines.upgrade', { machine_id: machine.id })}>Upgrade daemon</button></div>
  {:else}
    <div class="banner info"><span class="spin"></span> {machine.name}: {connLabel(machine)}</div>
  {/if}
{/each}
{#if s.state === 'dormant'}
  {#if s.resumable}
    <div class="banner">☾ Agent asleep <button class="btn sm" disabled={!online} onclick={resume}>Wake</button></div>
  {:else}
    <div class="banner warn">☾ The agent can't resume this session <button class="btn sm primary" disabled={!online} onclick={startFresh}>Start fresh…</button></div>
  {/if}
{:else if s.state === 'interrupted'}
  <div class="banner bad">⚡ Agent stopped mid-turn
    {#if s.resumable}<button class="btn sm primary" disabled={!online} onclick={resume}>Resume</button>{/if}
    <button class="btn sm" disabled={!online} onclick={startFresh}>Start fresh…</button>
  </div>
{:else if s.state === 'failed'}
  <div class="banner bad">✕ Turn failed{#if s.last_error}: {s.last_error}{/if}
    {#if s.unread}<button class="btn sm" disabled={!online} onclick={() => app.markRead(s)}>Dismiss</button>{/if}
  </div>
{:else if s.state === 'closed'}
  <div class="banner">Archived {s.closed_at ? ago(s.closed_at) : ''}
    <button class="btn sm" disabled={!online} onclick={() => app.unarchive(s)}>Unarchive</button>
    <button class="btn sm danger" disabled={!online} onclick={() => app.remove(s)}>Delete…</button>
  </div>
{/if}
{#if s.queue.send_now_pending}
  <div class="banner info"><span class="spin"></span> Sending when the running step finishes</div>
{/if}

<style>
  .banner { margin: 8px auto 0; max-width: 820px; width: calc(100% - 80px); padding: 8px 12px; border-radius: 8px; background: var(--bg3); font-size: 13px; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .warn { background: color-mix(in srgb, var(--warn) 13%, var(--bg2)); }
  .bad { background: color-mix(in srgb, var(--bad) 13%, var(--bg2)); }
  .info { background: color-mix(in srgb, var(--info) 12%, var(--bg2)); }
  .act { background: color-mix(in srgb, var(--accent) 8%, var(--bg2)); }
  @media (max-width: 760px) { .banner { width: auto; margin: 8px 12px 0; } }
</style>
