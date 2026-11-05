<script lang="ts">
  // Which events push (attention; non-blocking questions, off by default; finished runs), and Web
  // Push on this device.
  import { useApp } from '../app/app.svelte';
  import { describeError } from '../api/errors';
  import { pushSubscription } from '../pwa';
  import type { NotificationPreferences } from '../protocol';

  const app = useApp();
  const online = $derived(app.client.online);
  let prefs = $state<NotificationPreferences | null>(null);
  let permission = $state(typeof Notification === 'undefined' ? 'unsupported' : Notification.permission);
  $effect(() => {
    if (online) void app.client.call('notifications.preferences', {}).then((r) => (prefs = r ?? null));
  });
  const push = $derived(app.client.hello?.push ?? null);
  const thisDevice = $derived(app.client.feed.devices.get(app.client.device.id));
  const pushOn = $derived(app.client.feed.deviceList.filter((d) => d.push));

  async function set(fields: Partial<NotificationPreferences>) {
    const r = await app.client.call('notifications.set_preferences', fields);
    if (r) prefs = r;
  }
  async function enable() {
    if (!push || typeof Notification === 'undefined') return;
    permission = await Notification.requestPermission();
    if (permission !== 'granted') return app.toasts.show('Notifications were not allowed', 'error');
    try {
      const subscription = await pushSubscription(push.vapid_public_key);
      await app.client.call('notifications.subscribe', { device_id: app.client.device.id, subscription });
    } catch (e) {
      app.toasts.show(`Couldn't subscribe: ${describeError(e)}`, 'error');
    }
  }
</script>

{#if prefs}
  {@const p = prefs}
  <div class="sec-h">Push</div>
  <label class="check"><input type="checkbox" checked={p.attention} disabled={!online} onchange={(e) => set({ attention: e.currentTarget.checked })} /> Needs you, failed or interrupted</label>
  <label class="check"><input type="checkbox" checked={p.questions} disabled={!online} onchange={(e) => set({ questions: e.currentTarget.checked })} /> Questions</label>
  <label class="check"><input type="checkbox" checked={p.turn_end} disabled={!online} onchange={(e) => set({ turn_end: e.currentTarget.checked })} /> Finished runs</label>
{/if}
{#if push}
  <div class="row">
    <div>
      <b>This device</b>
      <div class="small muted">{thisDevice?.push ? 'On' : permission === 'denied' ? 'Blocked: allow notifications in the browser settings' : 'Off'}</div>
    </div>
    {#if thisDevice?.push}
      <button class="btn" disabled={!online} onclick={() => app.client.call('notifications.unsubscribe', { device_id: app.client.device.id })}>Turn off</button>
    {:else if permission !== 'denied' && permission !== 'unsupported'}
      <button class="btn primary" disabled={!online} onclick={enable}>Enable</button>
    {/if}
  </div>
  <div class="small muted">Push is on for {pushOn.length ? pushOn.map((d) => d.label).join(', ') : 'no devices'} · <button class="linkish" onclick={() => app.openSettings('devices')}>Devices…</button></div>
{:else}
  <div class="small muted">Web Push isn't configured on the server.</div>
{/if}

<style>
  .check { display: flex; align-items: center; gap: 8px; }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 14px 0; border-bottom: 1px solid var(--line); }
</style>
