<script lang="ts">
  // A new task in one go: title, status, priority, projects (pre-filled with the viewed project),
  // notes, checklist entries and linked sessions, all in one tasks.create.
  import { useApp } from '../app/app.svelte';
  import type { NewTaskPreset } from '../app/view.svelte';
  import { displayName } from '../model/filters';
  import { keyOf } from '../model/projects';
  import { PRIORITIES, TASK_COLUMNS } from '../model/tasks';
  import type { Priority, TaskStatus } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';
  import Modal from '../ui/Modal.svelte';
  import TagInput, { type Suggestion } from '../ui/TagInput.svelte';

  let { preset }: { preset: NewTaskPreset } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);

  let title = $state('');
  // svelte-ignore state_referenced_locally
  let status = $state<TaskStatus>(preset.status ?? 'todo');
  let priority = $state<Priority>('p2');
  // svelte-ignore state_referenced_locally
  let projects = $state<string[]>(preset.project_ids?.slice() ?? []);
  let notes = $state('');
  let items = $state<string[]>([]);
  let item = $state('');
  // svelte-ignore state_referenced_locally
  let sessions = $state<string[]>(preset.sessions?.slice() ?? []);
  let busy = $state(false);

  const projectSuggest = (q: string, picked: string[]): Suggestion[] =>
    feed.projectList.filter((p) => !picked.includes(p.id) && p.name.toLowerCase().includes(q.trim().toLowerCase())).map((p) => ({ value: p.id, label: p.name }));
  const sessionSuggest = (q: string, picked: string[]): Suggestion[] =>
    app.ctx.sessions
      .filter((s) => s.state !== 'closed' && !picked.includes(keyOf(s)) && displayName(s).toLowerCase().includes(q.trim().toLowerCase()))
      .sort((a, b) => b.updated_at - a.updated_at)
      .slice(0, 15)
      .map((s) => ({ value: keyOf(s), label: displayName(s), hint: feed.machines.get(s.machine_id)?.name }));

  function addItem(e: KeyboardEvent) {
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing || !item.trim()) return; // empty: Enter creates
    e.preventDefault();
    e.stopPropagation();
    items = [...items, item.trim()];
    item = '';
  }
  async function create() {
    if (busy) return;
    if (!title.trim()) return app.toasts.show('Give the task a title', 'error');
    busy = true;
    const links = sessions.map((k) => feed.sessions.get(k)).filter((s) => !!s).map((s) => ({ machine_id: s.machine_id, session_id: s.id }));
    const t = await app.client.call('tasks.create', {
      title: title.trim(),
      status,
      priority,
      project_ids: projects,
      notes: notes.trim(),
      checklist: item.trim() ? [...items, item.trim()] : items,
      links,
    });
    busy = false;
    if (!t) return;
    close();
    app.openTask(t.id);
  }
</script>

{#snippet projectChip(id: string)}
  {@const p = feed.projects.get(id)}
  <span class="pdot" style:background={p?.color ?? 'var(--muted)'}></span>{p?.name ?? id}
{/snippet}
{#snippet sessionChip(k: string)}
  {@const s = feed.sessions.get(k)}{s ? displayName(s) : 'missing session'}
{/snippet}

<Modal label="New task" oncancel={close} onprimary={create}>
  <h3>New task</h3>
  <label class="field"><span>Title</span><input placeholder="What needs doing?" bind:value={title} data-autofocus /></label>
  <div class="row2">
    <div class="field"><span>Status</span><Dropdown label="Status" value={status} options={TASK_COLUMNS.map((c) => ({ value: c.id, label: `${c.icon} ${c.label}` }))} onchange={(v) => (status = v as TaskStatus)} /></div>
    <div class="field"><span>Priority</span><Dropdown label="Priority" value={priority} options={PRIORITIES.map((p) => ({ value: p, label: p.toUpperCase() }))} onchange={(v) => (priority = v as Priority)} /></div>
  </div>
  <div class="field"><span>Projects</span><TagInput label="Projects" bind:values={projects} suggest={projectSuggest} chip={projectChip} placeholder="Add to a project…" empty="No more projects" /></div>
  <label class="field"><span>Notes</span><textarea data-doc rows="3" placeholder="Optional" bind:value={notes}></textarea></label>
  <div class="field">
    <span>Checklist</span>
    {#each items as it, i (i)}<div class="item"><span>☐ {it}</span><button class="btn xs ghost" aria-label="Remove item" onclick={() => (items = items.filter((_, j) => j !== i))}>✕</button></div>{/each}
    <input placeholder="+ add item (Enter)" aria-label="Add checklist item" bind:value={item} onkeydown={addItem} />
  </div>
  <div class="field"><span>Sessions</span><TagInput label="Linked sessions" bind:values={sessions} suggest={sessionSuggest} chip={sessionChip} placeholder="Link sessions…" empty="Type to find a session" /></div>
  <div class="acts">
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={busy || !app.client.online} onclick={create}><span class="kbd">↵</span> Create task</button>
  </div>
</Modal>

<style>
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .field > :global(.dd) { display: flex; }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .pdot { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 5px; }
  .item { display: flex; align-items: center; justify-content: space-between; font-size: 13px; color: var(--fg); }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  @media (max-width: 760px) { .row2 { grid-template-columns: 1fr; } }
</style>
