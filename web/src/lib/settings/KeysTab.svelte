<script lang="ts">
  // SSH keys: the controlroom key (fingerprint, public key, install command, rotate) and the
  // additional keys under Advanced (generate, import, delete).
  import { useApp } from '../app/app.svelte';
  import type { KeysListResult } from '../protocol';

  const app = useApp();
  const online = $derived(app.client.online);
  let keys = $state<KeysListResult | null>(null);
  let advanced = $state(false);
  let importing = $state(false);
  let importName = $state('');
  let privateKey = $state('');
  let passphrase = $state('');
  const reload = () => app.client.call('keys.list', {}).then((r) => (keys = r ?? null));
  $effect(() => {
    if (online) void reload();
  });

  const usedBy = (keyId: string | null) => app.client.feed.machineList.filter((m) => m.key_id === keyId).map((m) => m.name).join(', ') || 'no machines';

  async function rotate() {
    const ok = await app.dialogs.confirm({ title: 'Rotate the controlroom key?', body: 'A new key is installed on every reachable machine, then the old one is removed. Offline machines migrate when they reconnect.', ok: 'Rotate' });
    if (!ok) return;
    const r = await app.client.call('keys.rotate', {});
    if (!r) return;
    app.toasts.show(r.pending ? `Rotated on ${r.migrated} machines · ${r.pending} migrate when they reconnect` : `Rotated on ${r.migrated} machines`, 'ok');
    void reload();
  }
  async function generate() {
    const name = await app.dialogs.ask({ title: 'Generate key', value: 'controlroom', ok: 'Generate' });
    if (name && (await app.client.call('keys.generate', { name }))) void reload();
  }
  async function importKey() {
    if (!privateKey.trim()) return;
    const k = await app.client.call('keys.import', { name: importName.trim() || 'imported key', private_key: privateKey, passphrase: passphrase || null });
    if (!k) return;
    privateKey = passphrase = importName = '';
    importing = false;
    void reload();
  }
  async function readFile(e: Event & { currentTarget: HTMLInputElement }) {
    const f = e.currentTarget.files?.[0];
    if (!f) return;
    privateKey = await f.text();
    importName ||= f.name;
  }
  async function remove(id: string, name: string) {
    if (!(await app.dialogs.confirm({ title: `Delete ${name}?`, body: 'Machines using it need another key.', ok: 'Delete', danger: true }))) return;
    if (await app.client.call('keys.delete', { key_id: id })) void reload();
  }
</script>

{#if keys}
  {@const k = keys.controlroom}
  <div class="pk">
    <div class="h"><b>controlroom key</b><span class="muted small">{k.fingerprint} · used by {usedBy(null)}</span><span class="sp"></span><button class="btn sm" disabled={!online} onclick={rotate}>Rotate…</button></div>
    <div class="line"><code>{k.public_key}</code><button class="btn xs" onclick={() => app.copy(k.public_key)}>Copy</button></div>
    <div class="ib"><div class="small muted">Run this once on each host:</div><div class="line"><code>{k.install_command}</code><button class="btn xs" onclick={() => app.copy(k.install_command)}>Copy</button></div></div>
    {#if k.retiring_machines}<div class="small warn-t">The previous key stays until {k.retiring_machines} offline machine(s) reconnect.</div>{/if}
  </div>
  <button class="adv" aria-expanded={advanced} onclick={() => (advanced = !advanced)}>{advanced ? '▾' : '▸'} Advanced: additional keys{keys.additional.length ? ` (${keys.additional.length})` : ''}</button>
  {#if advanced}
    <div class="small muted">Only for hosts that can't accept the controlroom key; pick one per machine in Machines.</div>
    {#each keys.additional as x (x.id)}
      <div class="key">
        <b>{x.name}</b> <span class="muted small">{x.imported ? 'imported' : 'generated'} · {x.fingerprint} · used by {usedBy(x.id)}</span>
        <div class="line"><code>{x.public_key}</code><button class="btn xs" onclick={() => app.copy(x.public_key)}>Copy</button><button class="btn xs ghost bad-t" disabled={!online} onclick={() => remove(x.id, x.name)}>Delete</button></div>
      </div>
    {/each}
    {#if importing}
      <div class="import">
        <label class="field"><span>Name</span><input bind:value={importName} /></label>
        <label class="field"><span>Private key</span><textarea data-doc rows="4" spellcheck="false" placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" bind:value={privateKey}></textarea></label>
        <div class="row2">
          <label class="field"><span>Passphrase, if encrypted</span><input type="password" bind:value={passphrase} /></label>
          <label class="field"><span>Or a file</span><input type="file" onchange={readFile} /></label>
        </div>
        <div class="line"><button class="btn sm" onclick={() => (importing = false)}>Cancel</button><button class="btn sm primary" disabled={!privateKey.trim() || !online} onclick={importKey}>Import key</button></div>
      </div>
    {:else}
      <div class="line"><button class="btn sm" disabled={!online} onclick={generate}>Generate key</button><button class="btn sm" onclick={() => (importing = true)}>Import key…</button></div>
    {/if}
  {/if}
{:else}
  <div class="muted small">{online ? 'Loading…' : 'Not connected'}</div>
{/if}

<style>
  .pk { border: 1px solid var(--line); border-radius: 12px; padding: 14px; display: flex; flex-direction: column; gap: 10px; background: var(--bg2); }
  .h { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .line { display: flex; gap: 6px; align-items: center; }
  .line code { flex: 1; min-width: 0; overflow-x: auto; white-space: nowrap; font-size: 11.5px; }
  .ib { border-radius: 9px; background: var(--bg3); padding: 9px 10px; display: flex; flex-direction: column; gap: 6px; }
  .adv { align-self: flex-start; background: none; border: none; color: var(--muted); font: inherit; font-size: 13px; cursor: pointer; padding: 6px 0; }
  .key { border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; }
  .import { display: flex; flex-direction: column; gap: 8px; border: 1px solid var(--line); border-radius: 10px; padding: 12px; background: var(--bg3); }
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  textarea { font: 12px var(--mono); }
</style>
