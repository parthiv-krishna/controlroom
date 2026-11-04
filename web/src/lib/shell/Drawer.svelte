<script lang="ts">
  // Phone: the project switcher drawer (projects with stoplights, machines that need attention,
  // settings).
  import { useApp } from '../app/app.svelte';
  import { UNASSIGNED, health, projectCode, scopeName, type Scope } from '../model/projects';
  import { connLabel } from '../model/status';
  import Stoplight from '../ui/Stoplight.svelte';

  const app = useApp();
  const items = $derived<Scope[]>([null, ...app.client.feed.projectList, UNASSIGNED]);
  const idOf = (s: Scope) => (s === null || s === UNASSIGNED ? s : s.id);
  const down = $derived(app.client.feed.machineList.filter((m) => m.connectivity.state !== 'connected'));
</script>

<div class="bg" role="presentation" onmousedown={(e) => { if (e.target === e.currentTarget) app.view.drawer = false; }}>
  <div class="drawer" role="dialog" aria-label="Projects">
    <div class="h">Projects</div>
    {#each items as p (idOf(p) ?? 'all')}
      {@const real = p !== null && p !== UNASSIGNED ? p : null}
      <button class="p" class:on={app.view.project === idOf(p)} style:--pc={real?.color ?? 'var(--fg)'} onclick={() => app.openProject(idOf(p))}>
        <span class="ic" class:home={!real}>{real ? projectCode(real) : p === UNASSIGNED ? '○' : '∗'}</span><span class="nm">{scopeName(p)}</span>
        <Stoplight row h={health(app.ctx, p)} />
      </button>
    {/each}
    <button class="p add" onclick={() => { app.view.drawer = false; app.view.modal = { kind: 'project', id: null }; }}><span class="ic">+</span><span class="nm">New project</span></button>
    {#if down.length}
      <div class="h">Machines</div>
      {#each down as m (m.id)}
        <button class="p" onclick={() => app.openSettings('machines')}><span class="dot {m.connectivity.state}"></span><span class="nm">{m.name}</span><span class="small muted">{connLabel(m)}</span></button>
      {/each}
    {/if}
    <div class="h"></div>
    <button class="p" onclick={() => app.openSettings()}><span class="nm">Settings</span></button>
  </div>
</div>

<style>
  .bg { position: fixed; inset: 0; z-index: 80; background: rgba(0, 0, 0, 0.35); }
  .drawer { position: absolute; top: 0; left: 0; bottom: 0; width: min(320px, 86vw); background: var(--bg2); padding: calc(env(safe-area-inset-top) + 12px) 12px 24px; overflow-y: auto; display: flex; flex-direction: column; gap: 2px; }
  .h { font-size: 11px; text-transform: uppercase; letter-spacing: 0.07em; color: var(--muted); padding: 14px 8px 6px; }
  .p { display: flex; align-items: center; gap: 12px; background: none; border: none; color: var(--fg); font: inherit; font-size: 15px; padding: 9px 8px; border-radius: 10px; cursor: pointer; text-align: left; min-height: 48px; }
  .p.on { background: var(--bg3); font-weight: 600; }
  .p.add { color: var(--muted); }
  .ic { width: 32px; height: 32px; border-radius: 9px; display: grid; place-items: center; font-size: 12px; font-weight: 700; flex: none; text-transform: capitalize; background: color-mix(in srgb, var(--pc, var(--fg)) 16%, transparent); color: color-mix(in srgb, var(--pc, var(--fg)) 85%, var(--fg)); }
  .ic.home { background: var(--bg3); color: var(--muted); }
  .nm { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
