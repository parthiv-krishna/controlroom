<script lang="ts">
  // The picked profile's machine cards: connection, doctor results, key, tool-call grace,
  // auto-upgrade, retry/upgrade/remove, "+ Session here"; with several profiles "Shared with" and
  // move or share; and the add-machine flow (into the picked profile; on a fresh install the first
  // profile is created first).
  import { useApp } from '../app/app.svelte';
  import { sharedWith } from '../model/profiles';
  import { ago, connLabel, upgrading } from '../model/status';
  import { touches } from '../model/workspaces';
  import ProfileChip from '../profiles/ProfileChip.svelte';
  import type { KeysListResult, Machine, ProfileId } from '../protocol';
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

  const profile = $derived(app.settingsProfile);
  const machines = $derived(app.ctxOf(profile?.id).machineList);
  const sessionsOn = (m: Machine) => app.all.sessions.filter((s) => s.state !== 'closed' && touches(s, m.id)).length;
  /** How long a tool call waits for an unreachable machine before it fails (and its result comes late). */
  const GRACE: [string, number][] = [['1 min', 60], ['10 min', 600], ['1 h', 3600], ['1 day', 86400]];

  /** The profile a new machine joins: the picked one, or the first, created now. */
  let addTo = $state<ProfileId | null>(null);
  function add() {
    if (profile || !app.client.profilesSupported) return start(profile?.id ?? null);
    app.withProfile((id) => {
      app.view.settingsProfile = id;
      start(id);
    });
  }
  function start(id: ProfileId | null) {
    addTo = id;
    adding = true;
  }

  async function remove(m: Machine) {
    const body = 'Its workspaces become unavailable.';
    if (await app.dialogs.confirm({ title: `Remove ${m.name}?`, body, ok: 'Remove', danger: true })) await app.client.call('machines.remove', { machine_id: m.id });
  }
</script>

{#if adding}
  <AddMachine bind:this={wizard} {keys} profile={addTo} onclose={() => (adding = false)} />
{:else}
  {#each machines as m (m.id)}
    {@const st = m.connectivity.state}
    {@const shared = app.multi && profile ? sharedWith(m, app.profiles, profile.id) : []}
    <div class="card c-{st}" data-machine={m.id}>
      <div class="h"><span class="dot {st}"></span><b>{m.name}</b><span class="muted small">{m.user}@{m.host}:{m.port}</span>
        {#if shared.length}<span class="shared small muted" data-shared>Shared with {#each shared as p (p.id)}<ProfileChip profile={p} />{/each}</span>{/if}
        <span class="sp"></span>{#if st !== 'action_required'}<span class="chip">{connLabel(m)}</span>{/if}</div>
      <div class="small muted" data-version title="{m.arch ?? '?'}, {sessionsOn(m)} open session{sessionsOn(m) === 1 ? '' : 's'}">
        controlroomd {m.version ?? '—'}{#if st !== 'connected'}, last seen {ago(m.last_seen)}{/if}
        {#if m.update_available && st === 'connected'}<span class="upd">update available ({m.version} → {app.client.hello?.server_version ?? 'newer'})</span>{/if}
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
      <label class="check small"><input type="checkbox" checked={m.auto_upgrade} disabled={!online} onchange={(e) => app.client.call('machines.set_settings', { machine_id: m.id, auto_upgrade: e.currentTarget.checked })} /> Auto-upgrade</label>
      <div class="acts">
        {#if st === 'unreachable'}<button class="btn sm" aria-label="Retry connecting to {m.name}" disabled={!online} onclick={() => app.client.call('machines.reconnect', { machine_id: m.id })}>Retry</button>{/if}
        {#if st === 'incompatible'}<button class="btn sm primary" aria-label="Upgrade the daemon on {m.name}" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: m.id })}>Upgrade</button>
        {:else if st === 'connected' && m.update_available}<button class="btn sm" aria-label="Upgrade the daemon on {m.name}" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: m.id })}>Upgrade</button>{/if}
        {#if st === 'connected'}<button class="btn sm" onclick={() => { app.view.settings = null; app.newSession({ profile: profile?.id, workspaces: [{ machine_id: m.id, path: '~' }] }); }}>+ Session here</button>{/if}
        {#if app.multi && profile}
          {@const from = profile.id}
          <button class="btn sm ghost" aria-label="Move {m.name} to another profile" disabled={!online} onclick={() => app.moveToProfile({ from, machine_ids: [m.id] })}>Move to profile…</button>
          {#if !profile.private}<button class="btn sm ghost" aria-label="Share {m.name} with another profile" disabled={!online} onclick={() => app.moveToProfile({ from, machine_ids: [m.id], share: true })}>Share with…</button>{/if}
        {/if}
        <button class="btn sm ghost bad-t" disabled={!online} onclick={() => remove(m)}>Remove</button>
      </div>
    </div>
  {/each}
  <button class="btn primary add" disabled={!online} onclick={add}>+ Machine</button>
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
  .shared { display: inline-flex; align-items: center; gap: 5px; flex-wrap: wrap; }
  .upd { margin-left: 8px; color: var(--info); }
</style>
