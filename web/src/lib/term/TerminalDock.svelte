<script lang="ts">
  // The session's terminal dock (spec §22): one tab per shell, grouped by workspace (machine and
  // directory). The agent's bash runs in its shells, so their tabs show when the agent is running
  // a command or waits for input there. The "+" opens a terminal of the user's own: private (the
  // agent never sees it), sandboxed like the agent's shells or, confirmed, unsandboxed (marked red
  // on its tab and above the terminal). Closing the user's tab ends that terminal; closing the
  // dock only hides the tabs. On the desktop the dock sits below the composer, its tab strip
  // always visible, resizable from its top edge; on the phone the terminal opens full screen with
  // an extra-keys row. xterm.js is only loaded once a terminal opens.
  import { onSession, useApp } from '../app/app.svelte';
  import type { MachineId, Shell, Workspace } from '../protocol';
  import { DOCK_MIN_HEIGHT, docks } from './dock.svelte';
  import ExtraKeys from './ExtraKeys.svelte';
  import { passesKeys } from './keys';
  import type { LinkStatus } from './link';
  import { isMine, isUnsandboxed, tabLabel, unsandboxedConfirm } from './mine';
  import NewTerminal from './NewTerminal.svelte';
  import { shellGroups, type DockSession } from './session';
  import XTerm from './XTerm.svelte';

  let { session, machineName }: { session: DockSession; machineName: (id: MachineId) => string } = $props();
  const app = useApp();

  const st = $derived(docks.get(session.id));
  const groups = $derived(shellGroups(session, machineName));
  /** A terminal the user just opened that the summary doesn't list yet. */
  const opening = $derived(st.shell !== null && st.shell === st.opening && !session.shells.some((s) => s.name === st.shell));
  const cur = $derived(opening ? null : (session.shells.find((s) => s.name === st.shell) ?? session.shells[0] ?? null));
  // Selected for good once the summary lists it.
  $effect(() => {
    if (st.opening && session.shells.some((s) => s.name === st.opening)) docks.update(session.id, { opening: null });
  });
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

  /** The workspace a shell runs in, and its machine's SSH user. */
  function whereOf(shell: Shell): { machine: string; user: string | null } {
    const w = session.workspaces.find((x) => x.id === shell.workspace_id);
    const machine = w ? app.client.feed.machines.get(w.machine_id) : undefined;
    return { machine: w ? machineName(w.machine_id) : 'its machine', user: machine?.user ?? null };
  }

  async function openTerminal(w: Workspace, unsandboxed: boolean) {
    if (unsandboxed) {
      const user = app.client.feed.machines.get(w.machine_id)?.user ?? null;
      const ok = await app.dialogs.confirm({ title: 'Open an unsandboxed terminal?', body: unsandboxedConfirm(user, machineName(w.machine_id)), ok: 'Open unsandboxed', danger: true });
      if (!ok) return;
    }
    const shell = await app.client.call('session.terminal_open', {
      ...onSession(session),
      workspace_id: w.id,
      sandbox: unsandboxed ? 'unsandboxed' : 'workspace',
      confirm: unsandboxed,
    });
    if (shell) docks.update(session.id, { open: true, shell: shell.name, opening: shell.name });
  }

  /** Ends one of the user's terminals; asks first while a program runs in it. */
  async function closeTerminal(shell: Shell) {
    const busy = shell.name === cur?.name ? fg : shell.foreground;
    if (busy) {
      const ok = await app.dialogs.confirm({ title: `End terminal ${tabLabel(shell)}?`, body: `${busy} is still running in it.`, ok: 'End terminal', danger: true });
      if (!ok) return;
    }
    await app.client.call('session.terminal_close', { ...onSession(session), name: shell.name });
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
    class:raw={st.open && cur !== null && isUnsandboxed(cur)}
    style:height={st.open && app.desk ? `${st.height}px` : undefined}
    aria-label="Terminals"
    data-pane="terminal"
  >
    {#if st.open && app.desk}<div class="grip" role="separator" aria-orientation="horizontal" aria-label="Resize terminals" onpointerdown={drag}></div>{/if}
    <div class="tabs">
      {#if st.open && !app.desk}<button class="btn xs ghost" aria-label="Back" onclick={() => docks.update(session.id, { open: false })}>‹</button>{/if}
      <div class="strip" role="tablist" aria-label="Shells">
        {#each groups as g (g.label)}
          {#if groups.length > 1}<span class="grp" title={g.workspace?.root ?? g.label}>{g.label}</span>{/if}
          {#each g.shells as shell (shell.name)}
            {@const on = st.open && cur?.name === shell.name}
            {@const mine = isMine(shell)}
            {@const raw = isUnsandboxed(shell)}
            <span class="tabw" class:mine class:raw class:on>
              <button class="tab" role="tab" aria-selected={on} title="{g.label} · {shell.cwd}" onclick={() => pick(shell)}>
                {#if mine}<span class="who">{raw ? 'unsandboxed' : 'you'}</span>{:else}<span class="glyph" aria-hidden="true">◆</span>{/if}
                {tabLabel(shell)}
                {#if !mine}
                  {#if shell.state.state === 'waiting_input'}<span class="mark wait">⌨ input</span>
                  {:else if shell.state.state === 'running'}<span class="mark agent">agent</span>
                  {:else if shell.state.state === 'gone'}<span class="mark">exited</span>{/if}
                {/if}
              </button>
              {#if mine}<button class="x" aria-label="End terminal {tabLabel(shell)}" title="End this terminal" onclick={() => closeTerminal(shell)}>×</button>{/if}
            </span>
          {/each}
        {/each}
      </div>
      <NewTerminal workspaces={session.workspaces} {machineName} onpick={openTerminal} />
      <span class="sp"></span>
      {#if st.open}
        <button class="btn xs ghost" aria-label="Close terminals" title="Close" onclick={() => docks.update(session.id, { open: false })}>✕</button>
      {:else}
        <button class="btn xs ghost" onclick={() => docks.update(session.id, { open: true })}>Terminals</button>
      {/if}
    </div>
    {#if st.open && opening}
      <div class="banner" role="status">Opening terminal…</div>
    {:else if st.open && cur}
      {#if isUnsandboxed(cur)}
        {@const where = whereOf(cur)}
        <div class="banner unsandboxed" role="status">UNSANDBOXED · your terminal, as {where.user ?? 'your SSH user'} on {where.machine}</div>
      {:else if isMine(cur)}
        <div class="banner mine" role="status">Your terminal · sandboxed like the agent's shells · the agent can't see it</div>
      {/if}
      {#if !isMine(cur) && cur.state.state === 'waiting_input'}
        <div class="banner wait" role="status">⌨ <b>Waiting for input</b> <span class="small">· passwords aren't echoed</span></div>
      {/if}
      {#if status.state === 'detached'}
        <div class="banner" role="status">⚠ {status.reason}</div>
      {:else if status.state === 'ended'}
        <div class="banner" role="status" title={isMine(cur) ? undefined : "Reopens with the agent's next command"}>{status.reason} <button class="btn xs" onclick={() => xterm?.reconnect()}>Reconnect</button></div>
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
  .dock.raw { border-top: 2px solid var(--bad); }
  .dock.phone.open { position: fixed; inset: 0; z-index: 40; height: 100%; padding-top: env(safe-area-inset-top); }
  .dock.phone.raw { box-shadow: inset 0 0 0 2px var(--bad); }
  .grip { position: absolute; left: 0; right: 0; top: -3px; height: 6px; cursor: ns-resize; z-index: 2; }
  .tabs { position: relative; display: flex; align-items: center; gap: 4px; padding: 4px 8px; background: var(--bg2); border-bottom: 1px solid var(--line); flex: none; }
  .strip { display: flex; align-items: center; gap: 4px; overflow-x: auto; min-width: 0; }
  .grp { font-size: 11px; color: var(--muted); margin: 0 2px 0 8px; white-space: nowrap; }
  .grp:first-child { margin-left: 0; }
  .tabw { display: inline-flex; align-items: center; border-radius: 6px; border: 1px solid transparent; }
  .tabw:hover { background: var(--bg3); }
  .tabw.on { border-color: var(--line); background: var(--bg3); }
  .tabw.raw { border-color: color-mix(in srgb, var(--bad) 55%, transparent); background: color-mix(in srgb, var(--bad) 12%, transparent); }
  .tabw.raw.on { border-color: var(--bad); background: color-mix(in srgb, var(--bad) 22%, transparent); }
  .tab { display: inline-flex; align-items: center; gap: 6px; padding: 3px 10px; border: none; background: none; color: var(--fg); font: 12.5px var(--mono); cursor: pointer; white-space: nowrap; }
  .tabw.mine .tab { padding-right: 4px; }
  .glyph { font-size: 9px; color: var(--info); }
  .who { font: 600 10.5px var(--font); padding: 0 6px; border-radius: 999px; background: color-mix(in srgb, var(--accent) 22%, transparent); color: var(--fg); }
  .raw .who { background: var(--bad); color: #fff; text-transform: uppercase; letter-spacing: 0.03em; }
  .x { border: none; background: none; color: var(--muted); cursor: pointer; font-size: 14px; line-height: 1; padding: 2px 7px 2px 3px; }
  .x:hover { color: var(--fg); }
  .mark { font: 11px var(--font); color: var(--muted); }
  .mark.agent { color: var(--info); }
  .mark.wait { color: var(--warn); font-weight: 600; }
  .banner { padding: 4px 12px; font-size: 12.5px; background: #22252c; color: #d7dae0; display: flex; align-items: center; gap: 6px; flex: none; }
  .banner.wait { background: color-mix(in srgb, var(--warn) 30%, #22252c); }
  .banner.unsandboxed { background: var(--bad); color: #fff; font-weight: 600; letter-spacing: 0.02em; }
  .banner.mine { color: #9aa0ab; }
  .meta { padding: 2px 12px; color: #9aa0ab; background: #16181d; flex: none; }
  .meta.pass { color: #8fb4ff; }
</style>
