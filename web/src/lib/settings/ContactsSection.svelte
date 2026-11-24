<script lang="ts">
  // Contact grants (Settings › Sandbox): which sessions may message each other, and since when;
  // revocable. With several profiles, the picked profile's (grants never cross profiles).
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { ago } from '../model/status';
  import type { SessionId } from '../protocol';

  const app = useApp();
  const feed = app.client.feed;
  const online = $derived(app.client.online);
  const grants = $derived.by(() => {
    const pf = app.multi ? app.settingsProfile : null;
    return pf ? feed.grantList.filter((g) => feed.sessions.get(g.from)?.profile_id === pf.id) : feed.grantList;
  });
  const nameOf = (id: SessionId) => {
    const s = feed.sessions.get(id);
    return s ? displayName(s) : 'missing session';
  };
  function open(id: SessionId) {
    app.view.settings = null;
    app.open({ id });
  }
</script>

{#each grants as g (g.id)}
  {@const by = g.by ? (feed.devices.get(g.by.device_id)?.label ?? g.by.label) : null}
  <div class="row" title="Granted {ago(g.granted_at)}{by ? ` by ${by}` : ''}">
    <div class="pair"><button class="linkish" onclick={() => open(g.from)}>{nameOf(g.from)}</button>{' '}<span class="muted">⇄</span>{' '}<button class="linkish" onclick={() => open(g.to)}>{nameOf(g.to)}</button></div>
    <span class="chip">{g.scope === 'pair' ? 'this pair' : 'once'}</span>
    <button class="btn sm ghost bad-t" disabled={!online} onclick={() => app.client.call('grants.revoke', { grant_id: g.id })}>Revoke</button>
  </div>
{:else}
  <div class="small muted">None</div>
{/each}

<style>
  .row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .pair { flex: 1; min-width: 220px; }
</style>
