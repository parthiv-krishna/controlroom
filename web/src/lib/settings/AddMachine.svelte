<script lang="ts">
  // Add a machine: details (with the key's install command) → host key (machines.add) → confirm
  // (machines.add_confirm) → "permission denied" recovery with the install command → live install
  // progress (the new record's bootstrap in the state feed) → doctor results.
  import { useApp } from '../app/app.svelte';
  import { describeError } from '../api/errors';
  import { isLive } from '../model/status';
  import type { HostKeyInfo, KeysListResult } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';
  import DoctorView from './DoctorView.svelte';

  let { keys, onclose }: { keys: KeysListResult | null; onclose: () => void } = $props();
  const app = useApp();

  let step = $state<'form' | 'hostkey' | 'denied' | 'run'>('form');
  let host = $state('');
  let port = $state('22');
  let user = $state('');
  let name = $state('');
  let keyId = $state('');
  let hostKey = $state<HostKeyInfo | null>(null);
  let install = $state('');
  let error = $state('');
  let busy = $state(false);
  let machineId = $state<string | null>(null);
  let log = $state<string[]>([]);

  const added = $derived(machineId ? app.client.feed.machines.get(machineId) : undefined);
  const done = $derived(!!added && isLive(added) && !!added.doctor);
  const ready = $derived(!!host.trim() && !!user.trim());
  const installCommand = $derived(keyId ? null : keys?.controlroom.install_command);
  const details = () => ({ host: host.trim(), port: Number(port) || 22, user: user.trim(), name: name.trim() || null, key_id: keyId || null });

  $effect(() => {
    const msg = added?.bootstrap?.message;
    if (msg && log[log.length - 1] !== msg) log = [...log, msg];
  });

  async function run(f: () => Promise<void>) {
    if (busy) return;
    error = '';
    busy = true;
    try {
      await f();
    } catch (e) {
      error = describeError(e);
    }
    busy = false;
  }
  const connect = () =>
    run(async () => {
      if (!ready) return;
      hostKey = (await app.client.request('machines.add', details())).host_key;
      step = 'hostkey';
    });
  const trust = () =>
    run(async () => {
      if (!hostKey) return;
      const r = await app.client.request('machines.add_confirm', { ...details(), host_key_fingerprint: hostKey.fingerprint });
      if (r.outcome === 'permission_denied') {
        install = r.install_command;
        step = 'denied';
      } else if (r.outcome === 'added') {
        machineId = r.machine.id;
        step = 'run';
      }
    });

  /** Enter: the step's primary action. Esc: back one step. */
  export function primary() {
    if (step === 'form') void connect();
    else if (step === 'hostkey' || step === 'denied') void trust();
    else if (done) onclose();
  }
  export function back() {
    if (step === 'form' || (step === 'run' && done)) onclose();
    else if (step !== 'run') step = 'form';
  }
</script>

<div class="wiz">
  {#if step !== 'run'}<button class="back" onclick={onclose}>‹ Machines</button>{/if}
  {#if step === 'form'}
    <h4>Add machine</h4>
    <div class="row2">
      <label class="field"><span>Host</span><input bind:value={host} placeholder="devbox.example.com" data-autofocus /></label>
      <label class="field"><span>Port</span><input bind:value={port} inputmode="numeric" /></label>
    </div>
    <div class="row2">
      <label class="field"><span>User</span><input bind:value={user} placeholder="alice" /></label>
      <label class="field"><span>Display name</span><input bind:value={name} placeholder={host.split('.')[0]} /></label>
    </div>
    {#if keys?.additional.length}
      <div class="field"><span>Key</span><Dropdown label="SSH key" value={keyId} options={[{ value: '', label: 'controlroom key' }, ...keys.additional.map((k) => ({ value: k.id, label: k.name }))]} onchange={(v) => (keyId = v)} /></div>
    {/if}
    {#if installCommand}
      <div class="ib"><div class="small muted">Run this once on <b>{host || 'the host'}</b>{#if user} as <b>{user}</b>{/if}:</div><div class="cmd"><code>{installCommand}</code><button class="btn xs" onclick={() => app.copy(installCommand)}>Copy</button></div></div>
    {/if}
    {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
    <div class="acts"><button class="btn" onclick={onclose}><span class="kbd">Esc</span> Cancel</button><button class="btn primary" disabled={!ready || busy || !app.client.online} onclick={connect}>{#if busy}<span class="spin"></span>{:else}<span class="kbd">↵</span>{/if} Connect</button></div>
  {:else if step === 'hostkey' && hostKey}
    <h4>Confirm host key</h4>
    <p>Check that this matches <b>{host}</b>'s host key.</p>
    <pre class="fp">{hostKey.algorithm}  {hostKey.fingerprint}</pre>
    {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
    <div class="acts"><button class="btn" onclick={() => (step = 'form')}><span class="kbd">Esc</span> Back</button><button class="btn primary" disabled={busy} onclick={trust}>{#if busy}<span class="spin"></span>{:else}<span class="kbd">↵</span>{/if} Trust and continue</button></div>
  {:else if step === 'denied'}
    <h4>Permission denied</h4>
    <p><b>{user}@{host}</b> didn't accept the key.</p>
    <div class="ib"><div class="small muted">Run this on <b>{host}</b> as <b>{user}</b>, then try again:</div><div class="cmd"><code>{install}</code><button class="btn xs" onclick={() => app.copy(install)}>Copy</button></div></div>
    {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
    <div class="acts"><button class="btn" onclick={() => (step = 'form')}><span class="kbd">Esc</span> Edit details</button><button class="btn primary" disabled={busy} onclick={trust}>{#if busy}<span class="spin"></span>{:else}<span class="kbd">↵</span>{/if} Try again</button></div>
  {:else}
    <h4>Setting up {added?.name ?? host}</h4>
    <div class="log">
      {#each log as l, i (i)}<div class:ok={i < log.length - 1 || done}>{#if i < log.length - 1 || done}✓{:else}<span class="spin"></span>{/if} {l}</div>{/each}
      {#if added?.last_error}<div class="bad-t">✕ {added.last_error}</div>{/if}
    </div>
    {#if done && added?.doctor}<DoctorView doctor={added.doctor} /><div class="acts"><button class="btn primary" onclick={onclose}><span class="kbd">↵</span> Done</button></div>{/if}
  {/if}
</div>

<style>
  .wiz { display: flex; flex-direction: column; gap: 10px; max-width: 640px; }
  h4 { font-size: 16px; }
  p { margin: 0; }
  .back { align-self: flex-start; background: none; border: none; color: var(--muted); font: inherit; font-size: 13px; cursor: pointer; padding: 0; }
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .ib { border-radius: 9px; background: var(--bg3); padding: 9px 10px; display: flex; flex-direction: column; gap: 6px; }
  .cmd { display: flex; gap: 8px; align-items: center; }
  .cmd code { flex: 1; min-width: 0; overflow-x: auto; white-space: nowrap; font-size: 11.5px; background: none; }
  .fp { background: var(--bg3); padding: 10px; border-radius: 6px; font-size: 14px; }
  .log { font: 12.5px var(--mono); display: flex; flex-direction: column; gap: 4px; }
  .log .ok { color: var(--ok); }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
</style>
