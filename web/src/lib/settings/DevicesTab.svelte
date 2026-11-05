<script lang="ts">
  // Devices: every browser of the user (label, last seen, push, "this device"); rename, and remove,
  // which signs that device out everywhere (this one too, after confirming).
  import { useApp } from '../app/app.svelte';
  import { ago } from '../model/status';
  import type { Device } from '../protocol';
  import EditableTitle from '../ui/EditableTitle.svelte';

  const app = useApp();
  const me = $derived(app.client.device.id);
  const devices = $derived([...app.client.feed.deviceList].sort((a, b) => (a.device_id === me ? -1 : b.device_id === me ? 1 : b.last_seen - a.last_seen)));

  function rename(d: Device, label: string) {
    if (d.device_id === me) void app.client.nameDevice(label);
    else void app.client.call('devices.rename', { device_id: d.device_id, label });
  }
  async function remove(d: Device) {
    const mine = d.device_id === me;
    const ok = await app.dialogs.confirm(
      mine
        ? { title: 'Remove this device?', body: 'This signs out this browser.', ok: 'Remove and sign out', danger: true }
        : { title: `Remove ${d.label}?`, body: `${d.label} is signed out${d.push ? ' and stops getting push notifications' : ''}. Use this for a lost or old device.`, ok: 'Remove', danger: true },
    );
    if (ok) await app.client.call('devices.remove', { device_id: d.device_id });
  }
</script>

{#each devices as d (d.device_id)}
  <div class="row" data-device={d.device_id}>
    <div class="main">
      <div class="name"><EditableTitle value={d.label} editKey={d.device_id} onsave={(label) => rename(d, label)} />{#if d.device_id === me}<span class="this">this device</span>{/if}</div>
      <div class="small muted">{d.device_id === me ? 'active now' : `last seen ${ago(d.last_seen)}`} · added {ago(d.created_at)}</div>
    </div>
    <span class="push" class:on={d.push}>push {d.push ? 'on' : 'off'}</span>
    <button class="btn sm ghost bad-t" disabled={!app.client.online} onclick={() => remove(d)}>Remove</button>
  </div>
{:else}
  <div class="muted">No devices</div>
{/each}

<style>
  .row { display: flex; align-items: center; gap: 10px; padding: 10px 0; border-bottom: 1px solid var(--line); }
  .main { flex: 1; min-width: 0; }
  .name { display: flex; align-items: center; gap: 8px; font-weight: 600; }
  .this, .push { font-size: 11px; font-weight: 500; color: var(--muted); border: 1px solid var(--line); border-radius: 10px; padding: 0 6px; }
  .push.on { color: var(--ok); border-color: currentColor; }
</style>
