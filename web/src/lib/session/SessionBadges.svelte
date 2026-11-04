<script lang="ts">
  // Header badges: the sandbox level (popover with the session's grants; the "looser than
  // defaults" badge, `off` as the warning badge), peers (whom the session may contact), and "idle,
  // but live work" (jobs, subagents or open asks will start the next turn).
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { levelLabel } from '../model/sandbox';
  import { ago } from '../model/status';
  import type { FeedSession, SandboxGrant, SessionDetail } from '../protocol';
  import { dismissable } from '../ui/actions';
  import Icon from '../ui/Icon.svelte';

  let { s, detail }: { s: FeedSession; detail: SessionDetail | null } = $props();
  const app = useApp();
  let pop = $state<'sandbox' | 'peers' | null>(null);
  let sandboxAnchor: HTMLElement | undefined = $state();
  let peersAnchor: HTMLElement | undefined = $state();
  const close = () => (pop = null);

  const sb = $derived(s.sandbox);
  const lw = $derived(s.live_work);
  const idleLive = $derived((s.state === 'idle' || s.state === 'dormant') && lw.jobs + lw.children + lw.asks > 0);
  const liveText = $derived(
    [lw.jobs && `${lw.jobs} job${lw.jobs === 1 ? '' : 's'}`, lw.children && `${lw.children} subagent${lw.children === 1 ? '' : 's'}`, lw.asks && `${lw.asks} open ask${lw.asks === 1 ? '' : 's'}`]
      .filter(Boolean)
      .join(', '),
  );
  const REL: Record<string, string> = { task: 'same task', parent: 'parent', child: 'subagent', grant: 'granted', unknown: '' };
  const SCOPE: Record<string, string> = { once: 'once', session: 'this session', standing: 'standing', unknown: '' };

  async function revoke(g: SandboxGrant) {
    if (g.scope === 'standing') {
      pop = null;
      const ok = await app.dialogs.confirm({ title: `Remove the standing grant ${g.value}?`, body: 'The agent restarts with the new sandbox and resumes the conversation. A running turn is stopped.', ok: 'Remove and restart', danger: true });
      if (!ok) return;
    }
    await app.client.call('sandbox.revoke', { ...onSession(s), grant_id: g.id });
  }
  /** A peer's session, if the feed has it. */
  function openPeer(sessionId: string) {
    pop = null;
    if (app.client.feed.sessions.has(sessionId)) app.open({ id: sessionId });
  }
</script>

<span class="badges">
  <span class="wrap" bind:this={sandboxAnchor}>
    <button class="sbx lv-{sb.level}" class:looser={sb.looser_than_default} title={sb.looser_than_default ? `Looser than the defaults (${levelLabel(sb.default_level)})` : 'Sandbox'} aria-expanded={pop === 'sandbox'} onclick={() => (pop = pop === 'sandbox' ? null : 'sandbox')}>
      <Icon name={sb.level === 'off' ? 'shieldOff' : 'shield'} size={12} />{sb.level === 'off' ? 'no sandbox' : sb.level}{#if sb.grants.length}<span class="n">+{sb.grants.length}</span>{/if}
    </button>
    {#if pop === 'sandbox'}
      <div class="pop" role="dialog" aria-label="Sandbox" use:dismissable={{ close, anchor: sandboxAnchor }}>
        <b>Sandbox: {levelLabel(sb.level)}</b>
        <div class="small muted">{sb.explicit ? 'Chosen for this session' : 'From the machine and project defaults'}{#if sb.looser_than_default} · looser than the defaults ({levelLabel(sb.default_level)}){/if}</div>
        {#if sb.level === 'off'}<div class="off">Not sandboxed: the agent can read and write anything your user can.</div>{/if}
        <div class="sec-h">Grants</div>
        {#each sb.grants as g (g.id)}
          <div class="grant">
            <span class="gk">{g.kind}</span><code>{g.value}</code>{#if g.access}<span class="muted small">{g.access.replace('_', ' ')}</span>{/if}
            <span class="scope" class:standing={g.scope === 'standing'}>{SCOPE[g.scope]}</span><span class="muted small">{ago(g.granted_at)}</span>
            <span class="sp"></span>
            <button class="btn xs ghost bad-t" disabled={!app.client.online} onclick={() => revoke(g)}>{g.scope === 'standing' ? 'Remove · restarts agent' : 'Revoke'}</button>
          </div>
        {:else}
          <div class="small muted">No grants. Allowed sandbox requests appear here.</div>
        {/each}
        <button class="linkish small" onclick={() => { pop = null; app.openSettings('sandbox'); }}>Sandbox defaults…</button>
      </div>
    {/if}
  </span>
  {#if detail?.peers.length}
    {@const peers = detail.peers}
    <span class="wrap" bind:this={peersAnchor}>
      <button class="sbx" aria-expanded={pop === 'peers'} title="Sessions this agent may contact" onclick={() => (pop = pop === 'peers' ? null : 'peers')}>⇄ {peers.length} peer{peers.length === 1 ? '' : 's'}</button>
      {#if pop === 'peers'}
        <div class="pop" role="dialog" aria-label="Peers" use:dismissable={{ close, anchor: peersAnchor }}>
          <b>Peers</b>
          {#each peers as p (p.peer.session_id)}
            <button class="peer" onclick={() => openPeer(p.peer.session_id)}>
              <span class="pn">{p.peer.name}</span><span class="rel">{REL[p.relation]}{p.task ? `: ${p.task}` : ''}</span>
            </button>
          {/each}
          <div class="small muted">Anything else needs your approval.</div>
        </div>
      {/if}
    </span>
  {/if}
  {#if idleLive}<span class="sbx live" title="No run is active; this work starts the next turn when it finishes">◷ idle · {liveText} running</span>{/if}
</span>

<style>
  .badges { display: inline-flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .wrap { position: relative; }
  .sbx { display: inline-flex; align-items: center; gap: 4px; font: inherit; font-size: 11.5px; padding: 1px 8px; border-radius: 999px; border: 1px solid var(--line); background: none; color: var(--muted); cursor: pointer; }
  .sbx:hover { color: var(--fg); }
  .lv-strict { color: var(--ok); border-color: color-mix(in srgb, var(--ok) 35%, var(--line)); }
  .lv-relaxed { color: var(--info); border-color: color-mix(in srgb, var(--info) 35%, var(--line)); }
  .looser { border-style: dashed; }
  .lv-off { color: var(--bad); border-color: var(--bad); border-style: solid; font-weight: 650; background: color-mix(in srgb, var(--bad) 8%, transparent); }
  .n { opacity: 0.8; }
  .live { cursor: default; color: var(--info); border-style: dashed; }
  .pop { position: absolute; z-index: 60; top: calc(100% + 6px); left: 0; width: min(440px, 86vw); background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 10px 12px; font-size: 13px; display: flex; flex-direction: column; gap: 4px; }
  .off { background: color-mix(in srgb, var(--bad) 13%, var(--bg2)); border-radius: 6px; padding: 6px 8px; }
  .grant { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding: 3px 0; border-top: 1px solid var(--line); }
  .gk { font: 650 10.5px var(--mono); text-transform: uppercase; padding: 0 5px; border-radius: 5px; background: var(--bg3); }
  .scope { font-size: 10.5px; padding: 0 6px; border-radius: 999px; border: 1px solid var(--line); }
  .scope.standing { border-color: var(--accent); }
  .peer { display: flex; align-items: center; gap: 8px; width: 100%; background: none; border: none; font: inherit; color: var(--fg); text-align: left; padding: 5px 6px; border-radius: 7px; cursor: pointer; }
  .peer:hover { background: var(--hover); }
  .pn { font-weight: 600; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .rel { font-size: 11px; color: var(--muted); }
</style>
