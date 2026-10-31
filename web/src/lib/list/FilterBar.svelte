<script lang="ts">
  // Search, the two-pane Filter menu (State, Task, Location tree, Host, Agent), Group by, active
  // filter chips with "Save as view", and "+ New session".
  import { useApp } from '../app/app.svelte';
  import { FACETS, STATES, filterCount, type FacetKey } from '../model/filters';
  import { GROUPS } from '../model/grouping';
  import { locOptions, locTree } from '../model/location';
  import { inProject, taskIn } from '../model/projects';
  import { dismissable } from '../ui/actions';
  import Icon from '../ui/Icon.svelte';

  const app = useApp();
  const view = app.view;
  let bar: HTMLElement | undefined = $state();
  const n = $derived(filterCount(view.filters));
  const groupLabel = $derived(GROUPS.find(([g]) => g === app.prefs.group)?.[1] ?? 'Task');

  interface Opt {
    value: string;
    label: string;
    depth: number;
    /** A machine's connectivity dot. */
    dot?: string;
    host?: boolean;
  }
  const options = $derived.by((): Opt[] => {
    const ctx = app.ctx;
    switch (view.filterSection) {
      case 'state':
        return STATES.map(([value, label]) => ({ value, label, depth: 0 }));
      case 'task':
        return ctx.tasks
          .filter((t) => taskIn(t, app.project) && (t.status !== 'done' || view.filters.task.includes(t.id)))
          .map((t) => ({ value: t.id, label: t.title, depth: 0 }));
      case 'location': {
        const sessions = ctx.sessions.filter((s) => s.state !== 'closed' && inProject(ctx, s, app.project));
        return locOptions(locTree(sessions, ctx.machineList)).map((o) => ({ value: o.value, label: o.label, depth: o.depth, dot: o.host ? o.machine.connectivity.state : undefined, host: o.host }));
      }
      case 'machine':
        return ctx.machineList.map((m) => ({ value: m.id, label: m.name, depth: 0, dot: m.connectivity.state }));
      case 'agent':
        return [...new Set(ctx.sessions.map((s) => s.agent))].sort().map((a) => ({ value: a, label: a, depth: 0 }));
      default:
        return [];
    }
  });

  function toggle(value: string, on: boolean) {
    view.keep = [];
    const cur = view.filters[view.filterSection];
    view.filters[view.filterSection] = on ? [...cur, value] : cur.filter((x) => x !== value);
  }
  function remove(k: FacetKey, v: string) {
    view.keep = [];
    view.filters[k] = view.filters[k].filter((x) => x !== v);
  }
  function chipLabel(k: FacetKey, v: string): string {
    switch (k) {
      case 'location': return v.endsWith(':') ? v + '*' : v;
      case 'task': return app.client.feed.tasks.get(v)?.title ?? v;
      case 'machine': return app.client.feed.machines.get(v)?.name ?? v;
      case 'state': return STATES.find(([s]) => s === v)?.[1] ?? v;
      default: return v;
    }
  }
  const facetLabel = (k: FacetKey) => FACETS.find(([f]) => f === k)?.[1] ?? k;
  function searchKey(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      view.lastSearch = view.q.trim();
      (e.target as HTMLInputElement).blur();
    }
  }
</script>

<div class="bar" bind:this={bar}>
  <input id="session-search" class="search" placeholder="Search" aria-label="Search sessions" bind:value={view.q} oninput={() => (view.keep = [])} onkeydown={searchKey} />
  <button class="mbtn" class:open={view.menu === 'filter'} class:has={n > 0} aria-expanded={view.menu === 'filter'} onclick={() => (view.menu = view.menu === 'filter' ? null : 'filter')}>
    <Icon name="filter" size={13} /><span>Filter</span>{#if n}<b>{n}</b>{/if}
  </button>
  <button class="mbtn" class:open={view.menu === 'group'} aria-expanded={view.menu === 'group'} title="Group by (gr)" onclick={() => (view.menu = view.menu === 'group' ? null : 'group')}>
    <span class="muted">Group</span> {groupLabel} <span class="caret">▾</span>
  </button>
  {#if view.menu === 'filter'}
    <div class="menu fmenu" role="dialog" aria-label="Filter" use:dismissable={{ close: () => (view.menu = null), anchor: bar }}>
      <div class="sections">
        {#each FACETS as [k, label] (k)}
          <button class:on={view.filterSection === k} onclick={() => (view.filterSection = k)}>{label}{#if view.filters[k].length}<b>{view.filters[k].length}</b>{/if}</button>
        {/each}
        {#if n}<button class="clear" onclick={() => view.clearFilters()}>Clear all</button>{/if}
      </div>
      <div class="opts">
        {#each options as o (o.value)}
          <label style:padding-left="{7 + o.depth * 16}px">
            <input type="checkbox" checked={view.filters[view.filterSection].includes(o.value)} onchange={(e) => toggle(o.value, e.currentTarget.checked)} />
            {#if o.dot}<span class="dot {o.dot}"></span>{/if}
            <span class:loc={view.filterSection === 'location' && !o.host} class:host={o.host}>{o.label}</span>
          </label>
        {:else}
          <div class="small muted">Nothing to filter by</div>
        {/each}
      </div>
    </div>
  {:else if view.menu === 'group'}
    <div class="menu gmenu" role="menu" aria-label="Group by" use:dismissable={{ close: () => (view.menu = null), anchor: bar }}>
      {#each GROUPS as [g, label] (g)}
        <button role="menuitemradio" aria-checked={app.prefs.group === g} onclick={() => { app.prefs.group = g; view.menu = null; }}><span class="chk">{app.prefs.group === g ? '✓' : ''}</span>{label}</button>
      {/each}
    </div>
  {/if}
</div>
<div class="chips">
  <button class="new" onclick={() => app.newSessionHere()}>+ New session</button>
  {#each FACETS as [k] (k)}
    {#each view.filters[k] as v (v)}
      <span class="fchip"><span class="muted">{facetLabel(k)}</span><span class="v" class:loc={k === 'location'}>{chipLabel(k, v)}</span><button aria-label="Remove filter" onclick={() => remove(k, v)}>×</button></span>
    {/each}
  {/each}
  {#if view.filters.location.length}<button class="save" title="Save as a project view" onclick={() => (view.modal = { kind: 'project', id: null, paths: view.filters.location.slice() })}>Save as view</button>{/if}
</div>

<style>
  .bar { display: flex; gap: 6px; align-items: center; position: relative; }
  .search { flex: 1; min-width: 100px; border: none; background: var(--bg3); border-radius: 8px; padding: 6px 10px; }
  .mbtn { display: inline-flex; align-items: center; gap: 6px; background: transparent; border: 1px solid var(--line); color: var(--fg); border-radius: 8px; padding: 4px 10px; font: inherit; font-size: 12.5px; cursor: pointer; white-space: nowrap; }
  .mbtn:hover, .mbtn.open { border-color: var(--muted); }
  .mbtn.has { border-color: var(--fg); }
  .mbtn b { background: var(--fg); color: var(--bg); border-radius: 999px; font-size: 10.5px; padding: 0 6px; }
  .caret { color: var(--muted); font-size: 10px; }
  .menu { position: absolute; z-index: 40; top: 100%; margin-top: 6px; background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); }
  .fmenu { left: 0; right: 0; display: grid; grid-template-columns: 110px minmax(0, 1fr); max-height: 380px; overflow: hidden; }
  .sections { border-right: 1px solid var(--line); padding: 6px; display: flex; flex-direction: column; gap: 1px; }
  .sections button, .gmenu button { background: none; border: none; color: var(--fg); font: inherit; font-size: 13px; text-align: left; padding: 6px 9px; border-radius: 6px; cursor: pointer; display: flex; align-items: center; gap: 6px; }
  .sections button:hover, .gmenu button:hover { background: var(--bg3); }
  .sections button.on { background: var(--bg3); font-weight: 600; }
  .sections b { margin-left: auto; font-size: 11px; color: var(--muted); }
  .sections .clear { margin-top: auto; color: var(--muted); font-size: 12px; }
  .opts { overflow-y: auto; padding: 6px; }
  .opts label { display: flex; gap: 8px; align-items: center; padding: 5px 7px; border-radius: 6px; cursor: pointer; font-size: 13px; min-width: 0; }
  .opts label:hover { background: var(--bg3); }
  .opts label > span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .loc { font: 12px var(--mono); }
  .host { font-weight: 600; }
  .gmenu { right: 0; padding: 5px; min-width: 160px; display: flex; flex-direction: column; }
  .chk { width: 14px; }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; margin-top: 8px; }
  .new { display: inline-flex; align-items: center; gap: 4px; background: var(--fg); color: var(--bg); border: 1px solid var(--fg); border-radius: 8px; padding: 4px 11px; font: inherit; font-size: 12.5px; font-weight: 500; cursor: pointer; }
  .new:hover { opacity: 0.88; }
  .fchip { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; background: var(--bg3); border-radius: 999px; padding: 1px 3px 1px 9px; max-width: 100%; }
  .fchip .v { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 180px; }
  .fchip button { background: none; border: none; color: var(--muted); cursor: pointer; font: inherit; padding: 0 4px; }
  .save { background: none; border: none; color: var(--muted); font: inherit; font-size: 12px; cursor: pointer; text-decoration: underline; }
  @media (max-width: 760px) {
    .search { font-size: 16px; }
    .mbtn { padding: 8px 10px; }
    .fmenu, .gmenu { position: fixed; left: 0; right: 0; bottom: 0; top: auto; margin: 0; border-radius: 16px 16px 0 0; z-index: 90; padding-bottom: env(safe-area-inset-bottom); }
    .fmenu { max-height: 70dvh; }
    .sections button, .opts label, .gmenu button { min-height: 44px; font-size: 15px; }
    .new { padding: 8px 12px; font-size: 14px; }
  }
</style>
