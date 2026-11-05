<script lang="ts">
  // The open modal: new session, new task, project, start fresh, link to tasks, task detail.
  import { newOpId, useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { TASK_COLUMNS } from '../model/tasks';
  import TaskDetail from '../tasks/TaskDetail.svelte';
  import Modal from '../ui/Modal.svelte';
  import NewSession from './NewSession.svelte';
  import NewTask from './NewTask.svelte';
  import ProjectEditor from './ProjectEditor.svelte';

  const app = useApp();
  const md = $derived(app.view.modal);
  const close = () => (app.view.modal = null);
  const session = $derived(md && 'session' in md ? app.client.feed.sessions.get(md.session.id) : undefined);

  let includeHistory = $state(false);
  let filter = $state('');

  async function startFresh() {
    if (!session) return;
    const r = await app.client.call('session.start_fresh', { op_id: newOpId(), id: session.id, include_history: includeHistory });
    if (r) close();
  }
</script>

{#if md?.kind === 'new-session'}
  <NewSession preset={md.preset} />
{:else if md?.kind === 'new-task'}
  <NewTask preset={md.preset} />
{:else if md?.kind === 'project'}
  <ProjectEditor id={md.id} />
{:else if md?.kind === 'task'}
  {@const t = app.client.feed.tasks.get(md.id)}
  <Modal label="Task" oncancel={close}>{#if t}<TaskDetail {t} onclose={close} />{:else}<p class="muted">This task was deleted.</p>{/if}</Modal>
{:else if md?.kind === 'start-fresh' && session}
  <Modal label="Start fresh" oncancel={close} onprimary={startFresh}>
    <h3>Start fresh: {displayName(session)}</h3>
    <p>The agent forgets this conversation; the history stays.</p>
    <label class="check"><input type="checkbox" bind:checked={includeHistory} /> Pass the old conversation</label>
    <div class="acts">
      <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
      <button class="btn primary" disabled={!app.client.online} onclick={startFresh}><span class="kbd">↵</span> Start fresh</button>
    </div>
  </Modal>
{:else if md?.kind === 'link-tasks' && session}
  {@const s = session}
  <Modal label="Link to tasks" oncancel={close} onprimary={close}>
    <h3>Link “{displayName(s)}” to tasks</h3>
    <input placeholder="Filter tasks…" aria-label="Filter tasks" bind:value={filter} data-autofocus />
    <div class="list">
      {#each TASK_COLUMNS as c (c.id)}
        {@const ts = app.ctx.tasks.filter((t) => t.status === c.id && t.title.toLowerCase().includes(filter.toLowerCase()))}
        {#if ts.length}
          <div class="sec-h">{c.icon} {c.label}</div>
          {#each ts as t (t.id)}
            {@const on = t.links.includes(s.id)}
            <label class="check">
              <input type="checkbox" checked={on} disabled={!app.client.online} onchange={(e) => app.client.call(e.currentTarget.checked ? 'tasks.link' : 'tasks.unlink', { task_id: t.id, session_id: s.id })} />
              <span class="prio {t.priority}">{t.priority.toUpperCase()}</span> {t.title}
            </label>
          {/each}
        {/if}
      {/each}
    </div>
    <div class="acts">
      <button class="btn" onclick={() => app.newTask('doing', [s.id])}>+ New task</button>
      <button class="btn primary" onclick={close}><span class="kbd">↵</span> Done</button>
    </div>
  </Modal>
{/if}

<style>
  .check { display: flex; align-items: center; gap: 8px; padding: 3px 0; }
  .list { display: flex; flex-direction: column; max-height: 50vh; overflow: auto; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  p { margin: 0; }
</style>
