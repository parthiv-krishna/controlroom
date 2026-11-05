<script lang="ts">
  // The session's terminal dock (spec §22): one tab per shell, grouped by workspace (machine and
  // directory). The agent's bash runs in these shells, so a tab shows when the agent is running a
  // command or waits for input there. On the desktop the dock sits below the composer, its tab
  // strip always visible, resizable from its top edge; on the phone the terminal opens full screen
  // with an extra-keys row. xterm.js is only loaded once a terminal opens.
  import { useApp } from '../app/app.svelte';
  import type { MachineId, Shell } from '../protocol';
  import { DOCK_MIN_HEIGHT, docks } from './dock.svelte';
  import ExtraKeys from './ExtraKeys.svelte';
  import { passesKeys } from './keys';
  import type { LinkStatus } from './link';
  import { shellGroups, type DockSession } from './session';
  import XTerm from './XTerm.svelte';

  let { session, machineName }: { session: DockSession; machineName: (id: MachineId) => string } = $props();
  const app = useApp();

  const st = $derived(docks.get(session.id));
  const groups = $derived(shellGroups(session, machineName));
  const cur = $derived(session.shells.find((s) => s.name === st.shell) ?? session.shells[0] ?? null);
  const focusN = $derived.by(() => {
    const r = docks.focusRequest;
    return r && r.session === session.id && r.shell === cur?.name ? r.n : 0;
  });

  let status = $state<LinkStatus>({ state: 'connecting' });
  let foreground = $state<string | null>(null);
  let prefix = $state(false);
  let ctrl = $state(false);
  let xterm: XTerm | undefined = $state();
  /** The live foreground from the stream, else the summary's. */
  const fg = $derived(status.state === 'live' ? foreground : (cur?.foreground ?? null));

  function pick(shell: Shell) {
    docks.update(session.id, { open: true, shell: shell.name });
  }

  function drag(e: PointerEvent) {
    const y0 = e.clientY;
    const h0 = st.height;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const height = Math.max(DOCK_MIN_HEIGHT, Math.min(window.innerHeight * 0.8, h0 + y0 - ev.clientY));
      docks.update(session.id, { height });
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function extra(seq: string) {
    xterm?.send(seq);
    xterm?.focus();
  }

  const STATUS: Record<LinkStatus['state'], string> = {
    connecting: 'Connecting…',
    live: '',
    detached: '',
    offline: 'Connection lost · reconnecting…',
    ended: '',
    signed_out: 'Signed out',
  };
</script>

{#if session.shells.length}
  <section
    class="dock"
    class:open={st.open}
    class:phone={!app.desk}
    style:height={st.open && app.desk ? `${st.height}px` : undefined}
    aria-label="Terminals"
    data-pane="terminal"
  >
    {#if st.open && app.desk}<div class="grip" role="separator" aria-orientation="horizontal" aria-label="Resize terminals" onpointerdown={drag}></div>{/if}
    <div class="tabs" role="tablist" aria-label="Shells">
      {#if st.open && !app.desk}<button class="btn xs ghost" aria-label="Back" onclick={() => docks.update(session.id, { open: false })}>‹</button>{/if}
      {#each groups as g (g.label)}
        {#if groups.length > 1}<span class="grp" title={g.workspace?.root ?? g.label}>{g.label}</span>{/if}
        {#each g.shells as shell (shell.name)}
          {@const on = st.open && cur?.name === shell.name}
          <button class="tab" class:on role="tab" aria-selected={on} title="{g.label} · {shell.cwd}" onclick={() => pick(shell)}>
            {shell.name}
            {#if shell.state.state === 'waiting_input'}<span class="mark wait">⌨ input</span>
            {:else if shell.state.state === 'running'}<span class="mark agent">agent</span>
            {:else if shell.state.state === 'gone'}<span class="mark">exited</span>{/if}
          </button>
        {/each}
      {/each}
      <span class="sp"></span>
      {#if st.open}
        <button class="btn xs ghost" aria-label="Close terminals" title="Close" onclick={() => docks.update(session.id, { open: false })}>✕</button>
      {:else}
        <button class="btn xs ghost" onclick={() => docks.update(session.id, { open: true })}>Terminals</button>
      {/if}
    </div>
    {#if st.open && cur}
      {#if cur.state.state === 'waiting_input'}
        <div class="banner wait" role="status">⌨ <b>Waiting for input</b> <span class="small">· passwords aren't echoed</span></div>
      {/if}
      {#if status.state === 'detached'}
        <div class="banner" role="status">⚠ {status.reason}</div>
      {:else if status.state === 'ended'}
        <div class="banner" role="status" title="Reopens with the agent's next command">{status.reason} <button class="btn xs" onclick={() => xterm?.reconnect()}>Reconnect</button></div>
      {:else if STATUS[status.state]}
        <div class="banner" role="status">{STATUS[status.state]}</div>
      {/if}
      {#if fg}
        <div class="meta small" class:pass={passesKeys(fg)}>
          {fg} in the foreground · {passesKeys(fg) ? 'Ctrl-h/j/k/l go to it' : 'Ctrl-h/j/k/l move between panes'} · Ctrl-\ then a key always goes to the terminal
        </div>
      {/if}
      {#key cur.name}
        <XTerm bind:this={xterm} target={{ session: session.id, shell: cur.name }} shellState={cur.state.state} {focusN} bind:status bind:foreground bind:prefix bind:ctrl />
      {/key}
      {#if !app.desk}<ExtraKeys send={extra} bind:ctrl />{/if}
    {/if}
  </section>
{/if}

<style>
  .dock { position: relative; display: flex; flex-direction: column; flex: none; min-height: 0; border-top: 1px solid var(--line); background: var(--bg2); }
  .dock.open { background: #16181d; }
  .dock.phone.open { position: fixed; inset: 0; z-index: 40; height: 100%; padding-top: env(safe-area-inset-top); }
  .grip { position: absolute; left: 0; right: 0; top: -3px; height: 6px; cursor: ns-resize; z-index: 2; }
  .tabs { display: flex; align-items: center; gap: 4px; padding: 4px 8px; overflow-x: auto; background: var(--bg2); border-bottom: 1px solid var(--line); flex: none; }
  .grp { font-size: 11px; color: var(--muted); margin: 0 2px 0 8px; white-space: nowrap; }
  .grp:first-child { margin-left: 0; }
  .tab { display: inline-flex; align-items: center; gap: 6px; padding: 3px 10px; border-radius: 6px; border: 1px solid transparent; background: none; color: var(--fg); font: 12.5px var(--mono); cursor: pointer; white-space: nowrap; }
  .tab:hover { background: var(--bg3); }
  .tab.on { border-color: var(--line); background: var(--bg3); }
  .mark { font: 11px var(--font); color: var(--muted); }
  .mark.agent { color: var(--info); }
  .mark.wait { color: var(--warn); font-weight: 600; }
  .banner { padding: 4px 12px; font-size: 12.5px; background: #22252c; color: #d7dae0; display: flex; align-items: center; gap: 6px; flex: none; }
  .banner.wait { background: color-mix(in srgb, var(--warn) 30%, #22252c); }
  .meta { padding: 2px 12px; color: #9aa0ab; background: #16181d; flex: none; }
  .meta.pass { color: #8fb4ff; }
</style>
