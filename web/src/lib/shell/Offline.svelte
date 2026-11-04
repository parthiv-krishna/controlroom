<script lang="ts">
  // Shown until the first sync. A deep link from a push notification renders from its payload
  // (session name, machine, state) before any connection exists; the transport keeps retrying.
  import { useApp } from '../app/app.svelte';
  import type { DeepLink } from '../model/misc';
  import { ago } from '../model/status';

  let { link }: { link: DeepLink | null } = $props();
  const app = useApp();
  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const since = $derived(app.client.offlineSince);
</script>

<div class="offline" role="status">
  {#if link}
    <div class="card">
      <div class="small muted">Notification</div>
      <h2>{link.name ?? 'Session'}</h2>
      <div class="muted">{link.machine ?? ''}{link.state ? ` · ${link.state}` : ''}</div>
    </div>
  {/if}
  <div class="msg">
    {#if app.client.conn === 'connecting' && since === null}
      <span class="spin"></span><b>Connecting…</b>
    {:else}
      <span class="spin"></span>
      <div><b>Can't reach server</b><div class="small muted">Retrying automatically{since ? ` · since ${ago(since, now)}` : ''}</div></div>
    {/if}
  </div>
</div>

<style>
  .offline { position: fixed; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 22px; padding: 24px; }
  .card { background: var(--bg2); border: 1px solid var(--line); border-radius: 12px; padding: 16px 20px; min-width: min(360px, 90vw); }
  h2 { margin: 4px 0; font-size: 18px; }
  .msg { display: flex; gap: 12px; align-items: center; }
  .msg .spin { color: var(--muted); }
</style>
