<script lang="ts">
  // The agent daemon's session settings (every session runs on the server, so they are global):
  // idle stop and how long archived sessions are kept.
  import { newOpId, useApp } from '../app/app.svelte';
  import type { DaemonSettings } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';

  const app = useApp();
  const online = $derived(app.client.online);
  let server = $state<DaemonSettings | null>(null);
  $effect(() => {
    if (online) void app.client.call('settings.get', {}).then((r) => (server = r ?? null));
  });

  const IDLE = [1, 12, 24, 48, 168];
  const RETENTION: [string, number | null][] = [['forever', null], ['7 days', 7], ['30 days', 30], ['90 days', 90]];
  async function setServer(fields: { idle_timeout_h?: number; retention?: { delete_closed_after_days: number | null } }) {
    const r = await app.client.call('settings.set', { op_id: newOpId(), ...fields });
    if (r) server = r;
  }
</script>

{#if server}
  {@const ds = server}
  <div class="settings small" data-server>
    <span>Idle stop</span><Dropdown small label="Idle stop" value={String(ds.idle_timeout_h)} disabled={!online} options={[...new Set([...IDLE, ds.idle_timeout_h])].sort((a, b) => a - b).map((h) => ({ value: String(h), label: `${h} h` }))} onchange={(v) => setServer({ idle_timeout_h: Number(v) })} />
    <span>Keep archived</span><Dropdown small label="Retention" value={String(ds.retention.delete_closed_after_days)} disabled={!online} options={RETENTION.map(([label, d]) => ({ value: String(d), label }))} onchange={(v) => setServer({ retention: { delete_closed_after_days: v === 'null' ? null : Number(v) } })} />
  </div>
{/if}

<style>
  .settings { display: flex; align-items: center; gap: 6px 8px; flex-wrap: wrap; color: var(--muted); padding: 6px 0; }
  .settings span:not(:first-child) { margin-left: 8px; }
</style>
