<script lang="ts">
  // The small settings tabs: Contacts (grants), Schedules overview, Keyboard, Appearance.
  import { useApp } from '../app/app.svelte';
  import type { SettingsTab } from '../app/view.svelte';
  import type { Theme } from '../app/prefs.svelte';
  import { SHORTCUTS } from '../keys/shortcuts';
  import { displayName } from '../model/filters';
  import { formatIn, humanSchedule, runText } from '../model/schedule';
  import { ago } from '../model/status';
  import type { SessionId } from '../protocol';

  let { tab }: { tab: Extract<SettingsTab, 'contacts' | 'schedules' | 'keyboard' | 'appearance'> } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const online = $derived(app.client.online);

  const nameOf = (id: SessionId) => {
    const s = feed.sessions.get(id);
    return s ? displayName(s) : 'missing session';
  };
  function open(id: SessionId) {
    app.view.settings = null;
    app.open({ id });
  }
  const schedules = $derived([...feed.scheduleList].sort((a, b) => (a.enabled === b.enabled ? (a.next_run ?? Infinity) - (b.next_run ?? Infinity) : a.enabled ? -1 : 1)));
  const THEMES: [Theme, string][] = [['light', 'Light'], ['dark', 'Dark'], ['system', 'System']];
</script>

{#if tab === 'contacts'}
  <div class="small muted">Sessions on the same task, and a parent with its subagents, can always contact each other. Pairs you allowed beyond that are listed here.</div>
  {#each feed.grantList as g (g.id)}
    <div class="row">
      <div class="pair"><button class="linkish" onclick={() => open(g.from)}>{nameOf(g.from)}</button> <span class="muted">⇄</span> <button class="linkish" onclick={() => open(g.to)}>{nameOf(g.to)}</button></div>
      <span class="chip">{g.scope === 'pair' ? 'this pair' : 'once'}</span>
      <span class="small muted">granted {ago(g.granted_at)}{g.by ? ` · ${feed.devices.get(g.by.device_id)?.label ?? g.by.label}` : ''}</span>
      <button class="btn sm ghost bad-t" disabled={!online} onclick={() => app.client.call('grants.revoke', { grant_id: g.id })}>Revoke</button>
    </div>
  {:else}
    <div class="none">No contact grants</div>
  {/each}
{:else if tab === 'schedules'}
  <div class="small muted">Schedules wake a session's agent with a message. Add or edit them on the session.</div>
  {#each schedules as x (x.id)}
    {@const last = x.recent_runs[0]}
    <div class="row" class:off={!x.enabled} data-schedule={x.id}>
      <div class="pair">
        <div><b>{x.name}</b> <span class="small muted">{humanSchedule(x.spec)} · {x.timezone}</span></div>
        <div class="small muted">
          <button class="linkish" onclick={() => open(x.session_id)}>{nameOf(x.session_id)}</button>
          {#if x.enabled && x.next_run} · next {formatIn(x.next_run, x.timezone)}{/if}{#if last} · last <span class="o-{last.outcome}">{runText(last)}</span> {ago(last.until ?? last.at)}{/if}
        </div>
        {#if x.pending_run}<div class="small o-missed" data-pending>The {formatIn(x.pending_run, x.timezone)} run is waiting for the agent daemon</div>{/if}
      </div>
      <label class="check small"><input type="checkbox" checked={x.enabled} aria-label="Enable {x.name}" disabled={!online} onchange={(e) => app.client.call('schedules.update', { schedule_id: x.id, enabled: e.currentTarget.checked })} /> enabled</label>
    </div>
  {:else}
    <div class="none">No schedules</div>
  {/each}
{:else if tab === 'keyboard'}
  <label class="check"><input type="checkbox" bind:checked={app.prefs.keyboard.navigator} /> <span><b>Pane navigator</b>: Ctrl-h/j/k/l moves focus between panes, from text fields too</span></label>
  <label class="check"><input type="checkbox" bind:checked={app.prefs.keyboard.listVim} /> <span><b>Vim keys in lists</b>: gg/G, h/l, n/N, y, gr, : commands</span></label>
  <div class="sec-h">Shortcuts</div>
  <table class="small"><tbody>{#each SHORTCUTS as [k, what] (k)}<tr><td class="key">{k}</td><td>{what}</td></tr>{/each}</tbody></table>
{:else}
  <div class="row">
    <div class="pair"><b>Theme</b><div class="small muted">System follows the OS setting</div></div>
    <div class="seg" role="radiogroup" aria-label="Theme">
      {#each THEMES as [t, label] (t)}<button role="radio" aria-checked={app.prefs.theme === t} class:on={app.prefs.theme === t} onclick={() => (app.prefs.theme = t)}>{label}</button>{/each}
    </div>
  </div>
  {#if app.client.hello}
    {@const user = app.client.hello.user}
    <div class="row">
      <div class="pair"><b>Signed in</b><div class="small muted">{user.name ?? user.id}{user.email ? ` · ${user.email}` : ''} · server {app.client.hello.server_version}</div></div>
      {#if app.client.hello.auth === 'oidc'}<button class="btn" onclick={() => app.client.transport.logout()}>Sign out</button>{/if}
    </div>
  {/if}
{/if}

<style>
  .row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 10px 0; border-bottom: 1px solid var(--line); }
  .row.off { opacity: 0.6; }
  .o-missed, .o-failed { color: var(--bad); }
  .pair { flex: 1; min-width: 220px; }
  .none { color: var(--muted); text-align: center; padding: 40px 10px; }
  .check { display: flex; align-items: center; gap: 8px; }
  table { border-collapse: collapse; }
  td { padding: 4px 12px 4px 0; border-bottom: 1px solid var(--line); vertical-align: top; }
  .key { font-family: var(--mono); white-space: nowrap; }
  .seg { display: grid; grid-template-columns: repeat(3, 1fr); background: var(--bg3); border-radius: 8px; padding: 2px; width: 280px; }
  .seg button { background: none; border: none; color: var(--muted); font: inherit; font-size: 12.5px; padding: 5px; border-radius: 6px; cursor: pointer; }
  .seg button.on { background: var(--bg2); color: var(--fg); box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12); }
</style>
