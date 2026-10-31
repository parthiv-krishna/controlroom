<script lang="ts">
  // The project rail: ∗ all projects, each project's two-letter code with its stoplight, ＋ new
  // project view, ⚙ settings.
  import { useApp } from '../app/app.svelte';
  import { health, projectCode } from '../model/projects';
  import type { Project } from '../protocol';
  import Icon from '../ui/Icon.svelte';
  import Stoplight from '../ui/Stoplight.svelte';

  const app = useApp();
  const items = $derived<(Project | null)[]>([null, ...app.client.feed.projectList]);
</script>

<nav class="rail" aria-label="Projects" data-pane="rail">
  {#each items as p, i (p?.id ?? 'all')}
    {@const on = app.view.project === (p?.id ?? null)}
    <button class="pi" aria-label={p ? p.name : 'All projects'} aria-pressed={on} title={p ? p.name : 'All projects'} onclick={() => app.openProject(p?.id ?? null)}>
      <span class="code" class:on class:home={!p} style:--pc={p?.color ?? 'var(--fg)'}>{p ? projectCode(p) : '∗'}</span>
      <Stoplight h={health(app.ctx, p)} />
    </button>
    {#if i === 0}<span class="sep"></span>{/if}
  {/each}
  <button class="code add" title="New project view" aria-label="New project view" onclick={() => (app.view.modal = { kind: 'project', id: null })}><Icon name="plus" size={16} /></button>
  <span class="sp"></span>
  <button class="code add" title="Settings" aria-label="Settings" onclick={() => app.openSettings()}><Icon name="gear" size={18} /></button>
</nav>

<style>
  .rail {
    grid-area: rail; display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 14px 0 10px;
    overflow-y: auto; overflow-x: hidden; background: var(--rail-bg); border-right: 1px solid var(--line); min-height: 0;
  }
  .sep { width: 20px; border-top: 1px solid var(--line); flex: none; }
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
