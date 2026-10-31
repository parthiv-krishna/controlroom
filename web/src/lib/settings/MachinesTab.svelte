<script lang="ts">
  // Machine cards: connection, doctor results, idle stop and retention (daemon settings), key,
  // auto-upgrade, retry/upgrade/remove, "+ Session here"; and the add-machine flow.
  import { useApp } from '../app/app.svelte';
  import { ago, connLabel, isLive, upgrading } from '../model/status';
  import type { KeysListResult, Machine } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';
  import AddMachine from './AddMachine.svelte';
  import DoctorView from './DoctorView.svelte';
  import MachineFix from './MachineFix.svelte';

  let { adding = $bindable(), wizard = $bindable() }: { adding: boolean; wizard: AddMachine | undefined } = $props();
  const app = useApp();
  const online = $derived(app.client.online);
  let keys = $state<KeysListResult | null>(null);
  $effect(() => {
    if (online) void app.client.call('keys.list', {}).then((r) => (keys = r ?? null));
  });

  const sessionsOn = (m: Machine) => app.ctx.sessions.filter((s) => s.machine_id === m.id && s.state !== 'closed').length;
  const IDLE = [1, 12, 24, 48, 168];
  const RETENTION: [string, number | null][] = [['forever', null], ['7 days', 7], ['30 days', 30], ['90 days', 90]];
  const setDaemon = (m: Machine, fields: { idle_timeout_h?: number; retention?: { delete_closed_after_days: number | null } }) =>
    app.client.call('settings.set', { machine_id: m.id, op_id: crypto.randomUUID(), ...fields });

  async function remove(m: Machine) {
    if (await app.dialogs.confirm({ title: `Remove ${m.name}?`, body: 'Its sessions keep running, but disappear from controlroom.', ok: 'Remove', danger: true })) await app.client.call('machines.remove', { machine_id: m.id });
  }
</script>

{#if adding}
  <AddMachine bind:this={wizard} {keys} onclose={() => (adding = false)} />
{:else}
  {#each app.client.feed.machineList as m (m.id)}
    {@const st = m.connectivity.state}
    <div class="card c-{st}" data-machine={m.id}>
      <div class="h"><span class="dot {st}"></span><b>{m.name}</b><span class="muted small">{m.user}@{m.host}:{m.port}</span><span class="sp"></span><span class="chip">{connLabel(m)}</span></div>
      <div class="small muted">
        controlroomd {m.version ?? '—'} · {m.arch ?? '?'} · {sessionsOn(m)} sessions · {st === 'connected' ? 'live' : `last seen ${ago(m.last_seen)}`}
        {#if m.update_available && st === 'connected'} · update available ({m.version} → {app.client.hello?.server_version ?? 'newer'}){/if}
      </div>
      {#if st === 'action_required'}<MachineFix {m} />{:else if m.last_error && st !== 'connected'}<div class="small warn-t">{m.last_error}</div>{/if}
      {#if m.bootstrap}<div class="small"><span class="spin"></span> {upgrading(m) ? 'Upgrading: ' : ''}{m.bootstrap.message}</div>{/if}
      {#if m.doctor}<DoctorView doctor={m.doctor} />{/if}
      <div class="settings small">
        {#if m.settings}
          {@const ds = m.settings}
          Idle stop after <Dropdown small label="Idle stop" value={String(ds.idle_timeout_h)} disabled={!online || !isLive(m)} options={[...new Set([...IDLE, ds.idle_timeout_h])].sort((a, b) => a - b).map((h) => ({ value: String(h), label: `${h} h` }))} onchange={(v) => setDaemon(m, { idle_timeout_h: Number(v) })} />
          · Keep archived <Dropdown small label="Retention" value={String(ds.retention.delete_closed_after_days)} disabled={!online || !isLive(m)} options={RETENTION.map(([label, d]) => ({ value: String(d), label }))} onchange={(v) => setDaemon(m, { retention: { delete_closed_after_days: v === 'null' ? null : Number(v) } })} />
        {/if}
        {#if keys?.additional.length}
          · Key <Dropdown small label="SSH key" value={m.key_id ?? ''} disabled={!online} options={[{ value: '', label: 'controlroom key' }, ...keys.additional.map((k) => ({ value: k.id, label: k.name }))]} onchange={(v) => app.client.call('machines.set_settings', { machine_id: m.id, key: { key_id: v || null } })} />
        {/if}
      </div>
      <label class="check small"><input type="checkbox" checked={m.auto_upgrade} disabled={!online} onchange={(e) => app.client.call('machines.set_settings', { machine_id: m.id, auto_upgrade: e.currentTarget.checked })} /> Auto-upgrade the daemon while idle</label>
      <div class="acts">
        {#if st === 'unreachable'}<button class="btn sm" disabled={!online} onclick={() => app.client.call('machines.reconnect', { machine_id: m.id })}>Retry connection</button>{/if}
        {#if st === 'incompatible'}<button class="btn sm primary" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: m.id })}>Upgrade daemon</button>
        {:else if st === 'connected' && m.update_available}<button class="btn sm" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: m.id })}>Upgrade now</button>{/if}
        {#if st === 'connected'}<button class="btn sm" onclick={() => { app.view.settings = null; app.newSession({ machine_id: m.id }); }}>+ Session here</button>{/if}
        <button class="btn sm ghost bad-t" disabled={!online} onclick={() => remove(m)}>Remove</button>
      </div>
    </div>
  {/each}
  <button class="btn primary add" disabled={!online} onclick={() => (adding = true)}>+ Add machine</button>
{/if}

<style>
  .card { border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; background: var(--bg2); display: flex; flex-direction: column; gap: 6px; }
  .card.c-unreachable { border-color: color-mix(in srgb, var(--bad) 50%, var(--line)); }
  .card.c-incompatible { border-color: color-mix(in srgb, var(--warn) 50%, var(--line)); }
  .h { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .settings { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; color: var(--muted); }
  .check { display: flex; align-items: center; gap: 8px; color: var(--muted); }
  .acts { display: flex; gap: 6px; flex-wrap: wrap; }
  .add { align-self: flex-start; }
</style>
