<script lang="ts">
  // The inline fix for a machine whose connection needs the user: confirm a changed or unconfirmed
  // host key, install the key the host rejected, or stop the other controlroom server; then retry.
  import { useApp } from '../app/app.svelte';
  import type { Machine } from '../protocol';

  let { m, compact = false }: { m: Machine; compact?: boolean } = $props();
  const app = useApp();
  const c = $derived(m.connectivity);

  async function trust() {
    if (c.state !== 'action_required' || !c.presented_host_key) return;
    const hk = c.presented_host_key;
    if (c.reason === 'host_key_changed') {
      const ok = await app.dialogs.confirm({
        title: `Trust the new host key for ${m.name}?`,
        body: `New key: ${hk.algorithm} ${hk.fingerprint}. Trust it only if you expected the change.`,
        ok: 'Trust new key',
        danger: true,
      });
      if (!ok) return;
    }
    await app.client.call('machines.confirm_host_key', { machine_id: m.id, host_key_fingerprint: hk.fingerprint });
  }
</script>

{#if c.state === 'action_required'}
  <div class="fix">
    {#if c.reason === 'auth_failed'}
      <div><b>{m.user}@{m.host}</b> rejects the {m.key_id ? 'selected' : 'controlroom'} key. Reinstall it (SSH keys), then retry.</div>
      <div class="acts"><button class="btn sm primary" disabled={!app.client.online} onclick={() => app.client.call('machines.reconnect', { machine_id: m.id })}>Retry</button></div>
    {:else if c.reason === 'controller_conflict'}
      <div>Another controlroom server holds <b>{m.name}</b>. Stop it, then retry.</div>
      <div class="acts"><button class="btn sm primary" disabled={!app.client.online} onclick={() => app.client.call('machines.reconnect', { machine_id: m.id })}>Retry</button></div>
    {:else}
      <div>
        {#if c.reason === 'host_key_changed'}<b>{m.name}</b>: host key changed.{:else}Confirm <b>{m.name}</b>'s host key to connect.{/if}
      </div>
      {#if !compact}
        {#if m.host_key && c.reason === 'host_key_changed'}<div class="small muted">was <code>{m.host_key.fingerprint}</code></div>{/if}
        {#if c.presented_host_key}<pre class="fp">{c.presented_host_key.algorithm}  {c.presented_host_key.fingerprint}</pre>{/if}
      {/if}
      <div class="acts">
        <button class="btn sm" class:danger={c.reason === 'host_key_changed'} class:primary={c.reason !== 'host_key_changed'} disabled={!c.presented_host_key || !app.client.online} onclick={trust}>
          {c.reason === 'host_key_changed' ? 'Trust new key…' : 'Trust and connect'}
        </button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .fix { display: flex; flex-direction: column; gap: 6px; font-size: 13px; width: 100%; }
  .fp { background: var(--bg3); padding: 8px 10px; border-radius: 6px; font-size: 12.5px; }
  .acts { display: flex; gap: 6px; flex-wrap: wrap; }
</style>
