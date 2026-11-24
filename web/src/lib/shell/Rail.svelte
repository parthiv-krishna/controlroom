<script lang="ts">
  // The project rail: ∗ all projects, each project (a group of tasks) as its two-letter code with
  // its stoplight, ○ Unassigned (sessions no task's project claims), ＋ new project, machine status and
  // ⚙ settings. With several profiles the entries group by profile: a thin label (a lock when
  // private) and a colored left edge (full color on the current profile), the profile's ∗ and its
  // projects, and no Unassigned. Any entry enters its profile.
  import { useApp } from '../app/app.svelte';
  import { allOf, profileLabel } from '../model/profiles';
  import { UNASSIGNED, health, projectCode, scopeName, type Scope } from '../model/projects';
  import type { Profile, Project } from '../protocol';
  import Icon from '../ui/Icon.svelte';
  import MachineStatus from './MachineStatus.svelte';
  import Stoplight from '../ui/Stoplight.svelte';

  const app = useApp();
  const items = $derived<Scope[]>([null, ...app.ctx.projects, UNASSIGNED]);
  const idOf = (s: Scope) => (s === null || s === UNASSIGNED ? s : s.id);
</script>

{#snippet entry(pf: Profile, p: Project | null)}
  {@const pctx = app.ctxOf(pf.id)}
  {@const on = pf.id === app.profile?.id && (p ? app.project?.id === p.id : app.scope === null)}
  {@const name = p ? p.name : allOf(pf)}
  <button class="pi" aria-label={name} aria-pressed={on} title={name} onclick={() => app.openProject(p?.id ?? null, pf.id)}>
    <span class="code" class:on class:home={!p} style:--pc={p?.color ?? 'var(--fg)'}>{p ? projectCode(p) : '∗'}</span>
    <Stoplight h={health(pctx, p)} />
  </button>
{/snippet}

<nav class="rail" aria-label="Projects" data-pane="rail">
  {#if app.multi}
    {#each app.profiles as pf (pf.id)}
      <div class="grp" class:here={pf.id === app.profile?.id} style:--pf={pf.color} role="group" aria-label={profileLabel(pf)} data-profile={pf.id}>
        <span class="gl" title={profileLabel(pf)} aria-hidden="true">{#if pf.private}<Icon name="lock" size={9} />{/if}<span class="gn">{pf.name}</span></span>
        {@render entry(pf, null)}
        {#each app.ctxOf(pf.id).projects as p (p.id)}{@render entry(pf, p)}{/each}
      </div>
    {/each}
  {:else}
    {#each items as p, i (idOf(p) ?? 'all')}
      {@const on = app.view.project === idOf(p)}
      {@const real = p !== null && p !== UNASSIGNED ? p : null}
      <button class="pi" aria-label={scopeName(p)} aria-pressed={on} title={scopeName(p)} onclick={() => app.openProject(idOf(p))}>
        <span class="code" class:on class:home={!real} style:--pc={real?.color ?? 'var(--fg)'}>{real ? projectCode(real) : p === UNASSIGNED ? '○' : '∗'}</span>
        <Stoplight h={health(app.ctx, p)} />
      </button>
      {#if i === 0 || i === items.length - 2}<span class="sep"></span>{/if}
    {/each}
  {/if}
  <button class="code add" title="New project" aria-label="New project" onclick={() => app.newProject()}><Icon name="plus" size={16} /></button>
  <span class="sp"></span>
  <MachineStatus />
  <button class="code add" title="Settings" aria-label="Settings" onclick={() => app.openSettings()}><Icon name="gear" size={18} /></button>
</nav>

<style>
  .rail {
    grid-area: rail; display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 14px 0 10px;
    overflow-y: auto; overflow-x: hidden; background: var(--rail-bg); border-right: 1px solid var(--line); min-height: 0;
  }
  .sep { width: 20px; border-top: 1px solid var(--line); flex: none; }
  .grp { position: relative; display: flex; flex-direction: column; align-items: center; gap: 10px; align-self: stretch; padding: 0 0 4px; flex: none; }
  .grp::before { content: ''; position: absolute; left: 0; top: 0; bottom: 0; width: 3px; border-radius: 0 3px 3px 0; background: color-mix(in srgb, var(--pf) 35%, transparent); }
  .grp.here::before { background: var(--pf); }
  .gl {
    display: flex; align-items: center; justify-content: center; gap: 2px; max-width: calc(var(--rail-w) - 10px); margin-bottom: -4px;
    font-size: 10px; line-height: 1.2; color: var(--muted);
  }
  .grp.here .gl { color: color-mix(in srgb, var(--pf) 70%, var(--fg)); }
  .gn { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pi { display: flex; align-items: center; gap: 5px; padding: 2px; background: none; border: none; cursor: pointer; border-radius: 12px; color: inherit; }
  .code {
    width: 36px; height: 36px; border-radius: 10px; display: grid; place-items: center; flex: none; font: 650 12.5px var(--font);
    text-transform: capitalize; letter-spacing: 0.02em; background: color-mix(in srgb, var(--pc) 16%, transparent);
    color: color-mix(in srgb, var(--pc) 85%, var(--fg)); border: none;
  }
  .pi:hover .code { background: color-mix(in srgb, var(--pc) 26%, transparent); }
  .code.on { background: var(--pc); color: #fff; box-shadow: 0 2px 8px color-mix(in srgb, var(--pc) 40%, transparent); }
  .code.home { background: var(--bg3); color: var(--muted); font-size: 15px; }
  .code.home.on { background: var(--fg); color: var(--bg); box-shadow: none; }
  .code.add { border: 1px dashed var(--line); background: none; color: var(--muted); cursor: pointer; }
  .code.add:hover { color: var(--fg); }
</style>
