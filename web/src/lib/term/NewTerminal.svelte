<script lang="ts">
  // The dock's "+": opens a terminal of the user's own in one of the session's workspaces, in the
  // workspace's sandbox (like the agent's shells) or, after a confirm, unsandboxed. A small menu
  // on the desktop, full screen on the phone.
  import { useApp } from '../app/app.svelte';
  import type { MachineId, Workspace } from '../protocol';
  import { dismissable } from '../ui/actions';

  let {
    workspaces,
    machineName,
    onpick,
  }: {
    workspaces: Workspace[];
    machineName: (id: MachineId) => string;
    onpick: (workspace: Workspace, unsandboxed: boolean) => void;
  } = $props();
  const app = useApp();
  let open = $state(false);
  let anchor: HTMLButtonElement | undefined = $state();

  function pick(w: Workspace, unsandboxed: boolean) {
    open = false;
    onpick(w, unsandboxed);
  }
</script>

<span class="nt">
<button class="btn xs ghost plus" bind:this={anchor} aria-label="New terminal" title="New terminal" aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}>+</button>
{#if open}
  <div class="menu" class:phone={!app.desk} role="menu" aria-label="New terminal" use:dismissable={{ close: () => (open = false), anchor }}>
    {#if !app.desk}
      <div class="head"><b>New terminal</b><button class="btn xs ghost" aria-label="Cancel" onclick={() => (open = false)}>✕</button></div>
    {/if}
    {#each workspaces as w (w.id)}
      {@const label = `${machineName(w.machine_id)}:${w.path}`}
      <div class="row">
        <button class="item" role="menuitem" title="Sandboxed like the agent's shells" onclick={() => pick(w, false)}>{label}</button>
        <button class="item raw" role="menuitem" aria-label="Unsandboxed… {label}" title="Without the sandbox" onclick={() => pick(w, true)}>Unsandboxed…</button>
      </div>
    {/each}
  </div>
{/if}
</span>

<style>
  .nt { position: relative; display: inline-flex; flex: none; }
  .plus { font-size: 15px; line-height: 1; padding: 2px 8px; }
  .menu {
    position: absolute; bottom: calc(100% + 6px); right: 0; z-index: 50; min-width: 260px; max-width: min(460px, 90vw); background: var(--bg2);
    border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 4px; display: flex; flex-direction: column;
  }
  .menu.phone { position: fixed; inset: 0; max-width: none; border-radius: 0; z-index: 60; padding: calc(env(safe-area-inset-top) + 8px) 12px 12px; gap: 4px; }
  .head { display: flex; align-items: center; justify-content: space-between; padding: 4px 4px 10px; font-size: 15px; }
  .row { display: flex; align-items: stretch; gap: 4px; }
  .item { background: none; border: none; color: var(--fg); font: 12.5px var(--mono); text-align: left; padding: 6px 9px; border-radius: 6px; cursor: pointer; white-space: nowrap; }
  .item:first-child { flex: 1; overflow: hidden; text-overflow: ellipsis; }
  .item:hover { background: var(--bg3); }
  .item.raw { color: var(--bad); font-family: var(--font); }
  .phone .item { min-height: 44px; font-size: 15px; }
</style>
