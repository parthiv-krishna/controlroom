<script lang="ts">
  // The task detail: title, status, priority, projects, notes, checklist, linked sessions (with
  // their pending requests, answerable inline), start a session, link an existing one, delete.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { statusKind } from '../model/status';
  import { primaryWorkspace, wsLabel } from '../model/workspaces';
  import { PRIORITIES, TASK_COLUMNS, columnTasks, orderFor, rollup } from '../model/tasks';
  import type { Priority, Task, TaskStatus } from '../protocol';
  import RequestCard from '../session/RequestCard.svelte';
  import Dropdown from '../ui/Dropdown.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';
  import TagInput, { type Suggestion } from '../ui/TagInput.svelte';

  let { t, onclose }: { t: Task; onclose: () => void } = $props();
  const app = useApp();
  const r = $derived(rollup(app.ctx, t));
  const online = $derived(app.client.online);

  let title = $state('');
  let notes = $state('');
  let item = $state('');
  let lastId = '';
  $effect(() => {
    if (t.id === lastId) return;
    lastId = t.id;
    title = t.title;
    notes = t.notes;
  });

  const update = (fields: { title?: string; priority?: Priority; project_ids?: string[]; notes?: string }) => app.client.call('tasks.update', { task_id: t.id, ...fields });
  const moveTo = (status: TaskStatus) => app.client.call('tasks.move', { task_id: t.id, status, order: orderFor(columnTasks(app.ctx.tasks, status), t.id, null) });

  // Projects: read from the task, written straight to the server (replaces the set).
  const projectSuggest = (q: string, chosen: string[]): Suggestion[] =>
    app.client.feed.projectList.filter((p) => !chosen.includes(p.id) && p.name.toLowerCase().includes(q.trim().toLowerCase())).map((p) => ({ value: p.id, label: p.name }));
  function setProjects(ids: string[]) {
    if (ids.join() !== t.project_ids.join()) void update({ project_ids: ids });
  }

  function addItem(e: KeyboardEvent) {
    if (e.key !== 'Enter' || !item.trim()) return;
    e.preventDefault();
    e.stopPropagation();
    void app.client.call('tasks.checklist_add', { task_id: t.id, text: item.trim() });
    item = '';
  }
  const linkable = $derived(
    app.ctx.sessions.filter((s) => s.state !== 'closed' && !t.links.includes(s.id)),
  );
  async function remove() {
    if (!(await app.dialogs.confirm({ title: 'Delete task?', body: 'Linked sessions are kept.', ok: 'Delete', danger: true }))) return;
    if (await app.client.call('tasks.delete', { task_id: t.id })) onclose();
  }
</script>

{#snippet projectChip(id: string)}
  {@const p = app.client.feed.projects.get(id)}
  <span class="pdot" style:background={p?.color ?? 'var(--muted)'}></span>{p?.name ?? 'missing project'}
{/snippet}

<div class="td" aria-label="Task details" data-pane="task">
  <div class="th">
    <input class="title" aria-label="Task title" bind:value={title} onblur={() => title.trim() && title.trim() !== t.title && update({ title: title.trim() })} onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); e.currentTarget.blur(); } }} />
    <button class="btn sm ghost" aria-label="Close" onclick={onclose}>✕</button>
  </div>
  <div class="meta">
    <Dropdown small label="Status" value={t.status} disabled={!online} options={TASK_COLUMNS.map((c) => ({ value: c.id, label: `${c.icon} ${c.label}` }))} onchange={(v) => moveTo(v as TaskStatus)} />
    <Dropdown small label="Priority" value={t.priority} disabled={!online} options={PRIORITIES.map((p) => ({ value: p, label: p.toUpperCase() }))} onchange={(v) => update({ priority: v as Priority })} />
    <div class="projects"><TagInput label="Projects" bind:values={() => t.project_ids, setProjects} suggest={projectSuggest} chip={projectChip} placeholder="+ project" empty="No more projects" /></div>
  </div>
  <textarea data-doc rows="3" placeholder="Notes…" aria-label="Notes" bind:value={notes} onblur={() => notes !== t.notes && update({ notes })}></textarea>
  <div class="sec-h">Checklist</div>
  <div class="check">
    {#each t.checklist as c (c.id)}
      <div class="ck" class:done={c.done}>
        <label><input type="checkbox" checked={c.done} disabled={!online} onchange={() => app.client.call('tasks.checklist_update', { task_id: t.id, item_id: c.id, done: !c.done })} /> {c.text}</label>
        <button class="btn xs ghost" aria-label="Remove item" disabled={!online} onclick={() => app.client.call('tasks.checklist_remove', { task_id: t.id, item_id: c.id })}>✕</button>
      </div>
    {/each}
    <input placeholder="+ item" aria-label="Add checklist item" disabled={!online} bind:value={item} onkeydown={addItem} />
  </div>
  <div class="sec-h">Sessions <span>({r.sessions.length})</span></div>
  <div class="sessions">
    {#each r.sessions as s, i (s ? s.id : `missing-${i}`)}
      {@const link = t.links[i]}
      {#if s}
        {@const w = primaryWorkspace(s)}
        <div class="s" class:on={!!app.view.current && app.view.current.id === s.id} class:unread={s.unread} role="button" tabindex="0" onclick={(e) => { if (!(e.target as HTMLElement).closest('button, input, textarea')) app.open(s); }} onkeydown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) app.open(s); }}>
          <StatusIcon kind={statusKind(s)} />
          <div class="sm">
            <div class="sn">{displayName(s)}{#if s.todo} <span class="muted small">{s.todo.done}/{s.todo.total}</span>{/if}</div>
            <div class="sp-l">{w ? wsLabel(w, app.client.feed.machines) : ''}{#if s.workspaces.length > 1} +{s.workspaces.length - 1}{/if}{#if s.preview} · {s.preview}{/if}</div>
            {#each s.requests.filter((x) => x.state === 'pending') as req (req.id)}<RequestCard {req} {s} compact />{/each}
          </div>
          <button class="unlink" aria-label="Unlink {displayName(s)}" title="Unlink from this task" disabled={!online} onclick={() => app.client.call('tasks.unlink', { task_id: t.id, session_id: link })}>✕</button>
        </div>
      {:else}
        <div class="s missing">
          <span class="q">?</span><div class="sm"><div class="sn">Missing session</div></div>
          <button class="unlink" aria-label="Unlink missing session" disabled={!online} onclick={() => app.client.call('tasks.unlink', { task_id: t.id, session_id: link })}>✕</button>
        </div>
      {/if}
    {:else}
      <div class="muted small">No sessions yet</div>
    {/each}
  </div>
  <div class="acts">
    <button class="btn sm primary" onclick={() => app.startTask(t)}>▶ Start session</button>
    <Dropdown value="" placeholder="+ Link session…" label="Link existing session" disabled={!online} options={linkable.map((s) => ({ value: s.id, label: displayName(s), hint: app.client.feed.machines.get(primaryWorkspace(s)?.machine_id ?? '')?.name }))} onchange={(v) => void app.client.call('tasks.link', { task_id: t.id, session_id: v })} />
    <button class="btn sm ghost bad-t" disabled={!online} onclick={remove}>Delete task</button>
  </div>
</div>

<style>
  .td { display: flex; flex-direction: column; gap: 10px; padding: 14px 16px; overflow-y: auto; height: 100%; background: var(--bg2); }
  .td > * { flex-shrink: 0; }
  .th { display: flex; gap: 6px; align-items: center; }
  .title { font-size: 18px; font-weight: 700; border-color: transparent; background: transparent; padding: 4px 6px; }
  .title:hover { border-color: var(--line); }
  .meta { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .projects { flex: 1; min-width: 180px; }
  .pdot { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 5px; }
  .check { display: flex; flex-direction: column; gap: 3px; }
  .ck { display: flex; align-items: center; gap: 6px; }
  .ck label { flex: 1; display: flex; gap: 8px; align-items: center; font-size: 13.5px; cursor: pointer; }
  .ck.done label { color: var(--muted); text-decoration: line-through; }
  .check > input { padding: 4px 8px; font-size: 13px; }
  .sessions { display: flex; flex-direction: column; margin: 0 -8px; }
  .s { display: grid; grid-template-columns: 16px minmax(0, 1fr) auto; gap: 0 8px; align-items: start; border-radius: 9px; padding: 7px 8px; cursor: pointer; }
  .s:hover { background: var(--hover); }
  .s.on { background: var(--bg3); }
  .s.unread .sn { font-weight: 600; }
  .s.missing { cursor: default; color: var(--muted); font-style: italic; }
  .q { text-align: center; }
  .sm { min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .sn { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sp-l { font-size: 12px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .unlink { visibility: hidden; background: none; border: 1px solid transparent; border-radius: 6px; color: var(--muted); width: 22px; height: 22px; padding: 0; cursor: pointer; font-size: 11px; }
  .s:hover .unlink, .s:focus-within .unlink { visibility: visible; }
  @media (hover: none) { .unlink { visibility: visible; } }
  .acts { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; }
</style>
