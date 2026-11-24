<script lang="ts">
  // Settings › General: theme, the account, the server's session settings (idle stop, retention:
  // global, the agent daemon's), keyboard (pane navigation, vim keys, the shortcuts).
  import { useApp } from '../app/app.svelte';
  import type { Theme } from '../app/prefs.svelte';
  import { SHORTCUTS } from '../keys/shortcuts';
  import ServerSessions from './ServerSessions.svelte';

  const app = useApp();
  const THEMES: [Theme, string][] = [['light', 'Light'], ['dark', 'Dark'], ['system', 'System']];
</script>

<div class="row">
  <b class="pair">Theme</b>
  <div class="seg" role="radiogroup" aria-label="Theme">
    {#each THEMES as [t, label] (t)}<button role="radio" aria-checked={app.prefs.theme === t} class:on={app.prefs.theme === t} onclick={() => (app.prefs.theme = t)}>{label}</button>{/each}
  </div>
</div>
{#if app.client.hello}
  {@const hello = app.client.hello}
  <div class="row">
    <div class="pair" title="Server {hello.server_version}"><b>Signed in</b><div class="small muted">{hello.user.name ?? hello.user.id}{#if hello.user.email}{' '}({hello.user.email}){/if}</div></div>
    {#if hello.auth === 'oidc'}<button class="btn" onclick={() => app.client.transport.logout()}>Sign out</button>{/if}
  </div>
{/if}
<div class="sec-h">Sessions</div>
<ServerSessions />
<div class="sec-h">Keyboard</div>
<label class="check"><input type="checkbox" bind:checked={app.prefs.keyboard.navigator} /> <span><b>Pane navigation</b>{' '}<span class="muted">Ctrl-h/j/k/l</span></span></label>
<label class="check"><input type="checkbox" bind:checked={app.prefs.keyboard.listVim} /> <span><b>Vim keys in lists</b></span></label>
<table class="small"><tbody>{#each SHORTCUTS as [k, what] (k)}<tr><td class="key">{k}</td><td>{what}</td></tr>{/each}</tbody></table>

<style>
  .row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 10px 0; border-bottom: 1px solid var(--line); }
  .pair { flex: 1; min-width: 220px; }
  .check { display: flex; align-items: center; gap: 8px; }
  table { border-collapse: collapse; margin-top: 6px; }
  td { padding: 4px 12px 4px 0; border-bottom: 1px solid var(--line); vertical-align: top; }
  .key { font-family: var(--mono); white-space: nowrap; }
  .seg { display: grid; grid-template-columns: repeat(3, 1fr); background: var(--bg3); border-radius: 8px; padding: 2px; width: 280px; }
  .seg button { background: none; border: none; color: var(--muted); font: inherit; font-size: 12.5px; padding: 5px; border-radius: 6px; cursor: pointer; }
  .seg button.on { background: var(--bg2); color: var(--fg); box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12); }
</style>
