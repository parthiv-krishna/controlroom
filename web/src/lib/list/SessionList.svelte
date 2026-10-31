<script lang="ts">
  // The session list: project title, the device naming prompt, filters, then groups. Group headers
  // fold, task groups take a dropped session (link) and offer ✎ details and ＋ "new session here".
  // Subagents nest under their parent (dimmed ancestors keep the tree intact when filtering).
  import { useApp } from '../app/app.svelte';
  import type { Group } from '../model/grouping';
  import { keyOf } from '../model/projects';
  import { leadsToMatch, treeRoots } from '../model/tree';
  import type { MachineSession } from '../protocol';
  import EditableTitle from '../ui/EditableTitle.svelte';
  import Icon from '../ui/Icon.svelte';
  import DeviceNamePrompt from './DeviceNamePrompt.svelte';
  import FilterBar from './FilterBar.svelte';
  import SessionRow from './SessionRow.svelte';

  let { title = true }: { title?: boolean } = $props();
  const app = useApp();
  const view = app.view;
  const listing = $derived(app.listing);
  let over = $state<string | null>(null);

  function headClick(e: MouseEvent, key: string) {
    if (!(e.target as HTMLElement).closest('button')) view.collapsed[key] = !view.collapsed[key];
  }
  const carriesSession = (e: DragEvent) => !!e.dataTransfer?.types.includes('application/x-cr-session');
  function drop(e: DragEvent, g: Group) {
    over = null;
    const raw = e.dataTransfer?.getData('application/x-cr-session');
    if (!raw || !g.task) return;
    e.preventDefault();
    const link = JSON.parse(raw) as { machine_id: string; session_id: string };
    void app.client.call('tasks.link', { task_id: g.task.id, ...link });
  }
  const indentOf = (g: Group) => (g.depth !== undefined ? g.depth + 1 : 0);
</script>

{#snippet tree(s: MachineSession, indent: number, level: number, dim: boolean)}
  {@const fk = 'c:' + keyOf(s)}
  {@const kids = listing.tree.children(s).filter((c) => !dim || leadsToMatch(c, listing.visible, listing.tree))}
  <SessionRow {s} indent={indent + level} child={level > 0} kids={kids.length} folded={!!view.collapsed[fk]} onfold={() => (view.collapsed[fk] = !view.collapsed[fk])} ancestor={dim && !listing.visible.has(keyOf(s))} />
  {#if !view.collapsed[fk]}
    {#each kids as c (keyOf(c))}{@render tree(c, indent, level + 1, dim && !listing.visible.has(keyOf(c)))}{/each}
  {/if}
{/snippet}

<section class="list" aria-label="Sessions" data-pane="list">
  {#if title}
    <div class="lt">
      {#if app.project}
        {@const p = app.project}
        <span class="pdot" style:background={p.color}></span>
        <EditableTitle value={p.name} editKey={p.id} onsave={(name) => app.client.call('projects.update', { project_id: p.id, name })} />
        <button class="edit" title="Edit locations" aria-label="Edit project view" onclick={() => (view.modal = { kind: 'project', id: p.id })}><Icon name="sliders" /></button>
      {:else}
        <span class="all">All projects</span>
      {/if}
    </div>
  {/if}
  {#if !app.client.device.named}<DeviceNamePrompt />{/if}
  <div class="lh"><FilterBar /></div>
  <div class="rows" role="listbox" aria-label="Session list">
    {#each listing.groups as g (g.key)}
      {#if g.label !== null}
        <div
          class="g"
          class:host={g.depth === 0}
          class:node={!!g.depth}
          class:task={!!g.task}
          class:over={over === g.key}
          style:--depth={g.depth ?? 0}
          role="button"
          tabindex="-1"
          aria-expanded={!view.collapsed[g.key]}
          onclick={(e) => headClick(e, g.key)}
          onkeydown={(e) => { if (e.key === 'Enter') view.collapsed[g.key] = !view.collapsed[g.key]; }}
          ondragover={(e) => { if (g.task && carriesSession(e)) { e.preventDefault(); over = g.key; } }}
          ondragleave={() => (over = null)}
          ondrop={(e) => drop(e, g)}
          data-group={g.key}
        >
          <span class="fold">{view.collapsed[g.key] ? '▸' : '▾'}</span>
          {#if g.machine}<span class="dot {g.machine.connectivity.state}"></span>{/if}
          <span class="gt" class:loc={!!g.path} title={g.path}>{g.label}</span>
          {#if g.task}
            {@const t = g.task}
            <button class="gedit" title="Task details" aria-label="Task details" onclick={() => (view.modal = { kind: 'task', id: t.id })}><Icon name="pencil" size={12} /></button>
          {/if}
          {#if g.machine && g.machine.connectivity.state !== 'connected'}<span class="gst">{g.machine.connectivity.state.replace('_', ' ')}</span>{/if}
          <span class="sp"></span>
          {#if g.task}
            {@const t = g.task}
            <button class="gadd" title="New session for this task" aria-label="New session for this task" onclick={() => app.startTask(t)}>+</button>
          {:else if g.newAt}
            {@const at = g.newAt}
            <button class="gadd" title="New session here" aria-label="New session here" onclick={() => app.newSession({ machine_id: at.machine_id, cwd: at.cwd })}>+</button>
          {/if}
        </div>
      {/if}
      {#if !view.collapsed[g.key]}
        {#each treeRoots(g.items, listing.visible, listing.tree) as r (keyOf(r.s))}{@render tree(r.s, indentOf(g), 0, r.dim)}{/each}
        {#if g.task && !g.items.length}
          {@const t = g.task}
          <div class="gempty">Not started · <button class="linkish" onclick={() => app.startTask(t)}>Start</button></div>
        {/if}
      {/if}
    {:else}
      <div class="empty">Nothing matches.</div>
    {/each}
  </div>
</section>

<style>
  .list { grid-area: list; display: flex; flex-direction: column; min-height: 0; min-width: 0; background: var(--bg); }
  .lt { height: var(--top-h); display: flex; align-items: center; gap: 8px; padding: 0 14px; border-bottom: 1px solid var(--line); flex: none; font-weight: 600; font-size: 15px; min-width: 0; }
  .pdot { width: 10px; height: 10px; border-radius: 4px; flex: none; }
  .edit { background: none; border: none; color: var(--muted); cursor: pointer; padding: 3px 5px; border-radius: 6px; opacity: 0; }
  .lt:hover .edit, .edit:focus-visible { opacity: 1; }
  .edit:hover { background: var(--bg3); color: var(--fg); }
  .lh { padding: 14px 14px 10px; }
  .rows { flex: 1; overflow-y: auto; padding: 0 8px 48px; }
  .g { display: flex; align-items: center; gap: 7px; padding: 18px 10px 6px; padding-left: calc(10px + var(--depth) * 16px); font-size: 12.5px; cursor: pointer; border-radius: 8px; }
  .g.node { padding-top: 5px; padding-bottom: 3px; }
  .g.over { outline: 2px dashed var(--accent); outline-offset: -2px; }
  .fold { color: var(--muted); width: 10px; font-size: 11px; opacity: 0; }
  .g:hover .fold, .g[aria-expanded='false'] .fold { opacity: 1; }
  .gt { font-weight: 500; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .host .gt { color: var(--fg); font-weight: 600; }
  .g:hover .gt { color: var(--fg); }
  .gt.loc { font: 12px var(--mono); }
  .gst { font-size: 11px; color: var(--muted); }
  .gedit, .gadd { display: grid; place-items: center; width: 22px; height: 22px; flex: none; background: none; border: none; border-radius: 6px; color: var(--muted); cursor: pointer; padding: 0; font: inherit; visibility: hidden; }
  .g:hover .gedit, .g:hover .gadd { visibility: visible; }
  .gedit:hover, .gadd:hover { color: var(--fg); background: var(--bg3); }
  .gempty { font-size: 12px; color: var(--muted); padding: 2px 10px 8px 30px; }
  @media (hover: none) { .gedit, .gadd { visibility: visible; } }
</style>
