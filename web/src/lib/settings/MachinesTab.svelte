<script lang="ts">
  // The server's agent settings (idle stop, retention: the agent daemon's), then machine cards:
  // connection, doctor results, key, tool-call grace, auto-upgrade, retry/upgrade/remove,
  // "+ Session here"; and the add-machine flow.
  import { newOpId, useApp } from '../app/app.svelte';
  import { ago, connLabel, upgrading } from '../model/status';
  import { touches } from '../model/workspaces';
  import type { DaemonSettings, KeysListResult, Machine } from '../protocol';
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

  /** The agent daemon's settings (every session runs on the server). */
  let server = $state<DaemonSettings | null>(null);
  $effect(() => {
    if (online) void app.client.call('settings.get', {}).then((r) => (server = r ?? null));
  });

  const sessionsOn = (m: Machine) => app.ctx.sessions.filter((s) => s.state !== 'closed' && touches(s, m.id)).length;
  const IDLE = [1, 12, 24, 48, 168];
  const RETENTION: [string, number | null][] = [['forever', null], ['7 days', 7], ['30 days', 30], ['90 days', 90]];
  /** How long a tool call waits for an unreachable machine before it fails (and its result comes late). */
  const GRACE: [string, number][] = [['1 min', 60], ['10 min', 600], ['1 h', 3600], ['1 day', 86400]];
  async function setServer(fields: { idle_timeout_h?: number; retention?: { delete_closed_after_days: number | null } }) {
    const r = await app.client.call('settings.set', { op_id: newOpId(), ...fields });
    if (r) server = r;
  }

  async function remove(m: Machine) {
    const body = 'Sessions keep running; their workspaces there become unavailable.';
    if (await app.dialogs.confirm({ title: `Remove ${m.name}?`, body, ok: 'Remove', danger: true })) await app.client.call('machines.remove', { machine_id: m.id });
  }
</script>

{#if adding}
  <AddMachine bind:this={wizard} {keys} onclose={() => (adding = false)} />
{:else}
  {#if server}
    {@const ds = server}
    <div class="card" data-server>
      <div class="h"><b>Agents</b><span class="muted small">on the server</span></div>
      <div class="settings small">
        <span>Idle stop after</span><Dropdown small label="Idle stop" value={String(ds.idle_timeout_h)} disabled={!online} options={[...new Set([...IDLE, ds.idle_timeout_h])].sort((a, b) => a - b).map((h) => ({ value: String(h), label: `${h} h` }))} onchange={(v) => setServer({ idle_timeout_h: Number(v) })} />
        <span>Keep archived</span><Dropdown small label="Retention" value={String(ds.retention.delete_closed_after_days)} disabled={!online} options={RETENTION.map(([label, d]) => ({ value: String(d), label }))} onchange={(v) => setServer({ retention: { delete_closed_after_days: v === 'null' ? null : Number(v) } })} />
      </div>
    </div>
  {/if}
  {#each app.client.feed.machineList as m (m.id)}
    {@const st = m.connectivity.state}
    <div class="card c-{st}" data-machine={m.id}>
      <div class="h"><span class="dot {st}"></span><b>{m.name}</b><span class="muted small">{m.user}@{m.host}:{m.port}</span><span class="sp"></span><span class="chip">{connLabel(m)}</span></div>
      <div class="small muted">
        controlroomd {m.version ?? '—'} · {m.arch ?? '?'} · {sessionsOn(m)} sessions · {st === 'connected' ? 'live' : `last seen ${ago(m.last_seen)}`}
        {#if m.update_available && st === 'connected'} · update available ({m.version} → {app.client.hello?.server_version ?? 'newer'}){/if}
      </div>
      {#if st === 'action_required'}<MachineFix {m} />{:else if m.last_error}<div class="small warn-t" role="status">{m.last_error}</div>{/if}
      {#if m.bootstrap}<div class="small"><span class="spin"></span> {upgrading(m) ? 'Upgrading: ' : ''}{m.bootstrap.message}</div>{/if}
      {#if m.doctor}<DoctorView doctor={m.doctor} />{/if}
      <div class="settings small">
        <span title="How long a tool call waits for this machine before it fails">Link-loss grace</span><Dropdown small label="Link-loss grace" value={String(m.grace_s)} disabled={!online} options={[...GRACE.filter(([, s]) => s !== m.grace_s), [`${m.grace_s} s`, m.grace_s] as [string, number]].sort((a, b) => a[1] - b[1]).map(([label, s]) => ({ value: String(s), label: GRACE.find(([, g]) => g === s)?.[0] ?? label }))} onchange={(v) => app.client.call('machines.set_settings', { machine_id: m.id, grace_s: Number(v) })} />
        {#if keys?.additional.length}
          <span>Key</span><Dropdown small label="SSH key" value={m.key_id ?? ''} disabled={!online} options={[{ value: '', label: 'controlroom key' }, ...keys.additional.map((k) => ({ value: k.id, label: k.name }))]} onchange={(v) => app.client.call('machines.set_settings', { machine_id: m.id, key: { key_id: v || null } })} />
        {/if}
      </div>
      <label class="check small"><input type="checkbox" checked={m.auto_upgrade} disabled={!online} onchange={(e) => app.client.call('machines.set_settings', { machine_id: m.id, auto_upgrade: e.currentTarget.checked })} /> Auto-upgrade the daemon while idle</label>
      <div class="acts">
        {#if st === 'unreachable'}<button class="btn sm" disabled={!online} onclick={() => app.client.call('machines.reconnect', { machine_id: m.id })}>Retry connection</button>{/if}
        {#if st === 'incompatible'}<button class="btn sm primary" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: m.id })}>Upgrade daemon</button>
        {:else if st === 'connected' && m.update_available}<button class="btn sm" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: m.id })}>Upgrade now</button>{/if}
        {#if st === 'connected'}<button class="btn sm" onclick={() => { app.view.settings = null; app.newSession({ workspaces: [{ machine_id: m.id, path: '~' }] }); }}>+ Session here</button>{/if}
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
  .settings { display: flex; align-items: center; gap: 6px 8px; flex-wrap: wrap; color: var(--muted); }
  .settings span:not(:first-child) { margin-left: 8px; }
  .check { display: flex; align-items: center; gap: 8px; color: var(--muted); }
  .acts { display: flex; gap: 6px; flex-wrap: wrap; }
  .add { align-self: flex-start; }
</style>
