<script lang="ts">
  // A project: a named, colored group of tasks. Sessions belong to it through the tasks that link
  // them; the preview shows what it holds now.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import Modal from '../ui/Modal.svelte';

  let { id }: { id: string | null } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);
  const COLORS = ['#f97316', '#facc15', '#34d399', '#22d3ee', '#60a5fa', '#a78bfa', '#f472b6', '#f43f5e'];

  // svelte-ignore state_referenced_locally
  const base = id ? feed.projects.get(id) : undefined;
  let name = $state(base?.name ?? '');
  let color = $state(base?.color ?? COLORS[feed.projects.size % COLORS.length]);

  const preview = $derived({
    sessions: id ? app.ctx.sessions.filter((s) => s.state !== 'closed' && s.project_ids.includes(id)) : [],
    tasks: id ? app.ctx.tasks.filter((t) => t.status !== 'done' && t.project_ids.includes(id)) : [],
  });

  async function save() {
    const fields = { name: name.trim() || 'Untitled', color };
    const p = id ? await app.client.call('projects.update', { project_id: id, ...fields }) : await app.client.call('projects.create', fields);
    if (!p) return;
    close();
    app.openProject(p.id);
  }
  async function remove() {
    if (!id || !(await app.dialogs.confirm({ title: 'Delete project?', body: 'Its tasks stay; they just leave the project.', ok: 'Delete', danger: true }))) return;
    if (await app.client.call('projects.delete', { project_id: id })) {
      close();
      app.openProject(null);
    }
  }
</script>

<Modal label="Project" oncancel={close} onprimary={save}>
  <h3>{id ? 'Edit' : 'New'} project</h3>
  <div class="row2">
    <label class="field"><span>Name</span><input bind:value={name} data-autofocus /></label>
    <div class="field">
      <span>Color</span>
      <div class="swatches" role="radiogroup" aria-label="Color">
        {#each COLORS as c (c)}<button class="swatch" class:on={color === c} style:--sw={c} role="radio" aria-checked={color === c} aria-label={c} onclick={() => (color = c)}></button>{/each}
      </div>
    </div>
  </div>
  <div class="preview">
    {#if id}
      Holds <b>{preview.tasks.length}</b> open task{preview.tasks.length === 1 ? '' : 's'} and their <b>{preview.sessions.length}</b> session{preview.sessions.length === 1 ? '' : 's'}
    {:else}
      Add tasks to the project; the sessions linked to them join it.
    {/if}
    {#if preview.sessions.length}<div class="names">{preview.sessions.slice(0, 6).map(displayName).join(' · ')}{preview.sessions.length > 6 ? ` · +${preview.sessions.length - 6} more` : ''}</div>{/if}
  </div>
  <div class="acts">
    {#if id}<button class="btn ghost bad-t del" onclick={remove}>Delete project</button>{/if}
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={!app.client.online} onclick={save}><span class="kbd">↵</span> Save</button>
  </div>
</Modal>

<style>
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .swatches { display: flex; gap: 6px; flex-wrap: wrap; padding: 6px 0; }
  .swatch { width: 22px; height: 22px; border-radius: 50%; border: 2px solid transparent; background: var(--sw); cursor: pointer; padding: 0; box-shadow: inset 0 0 0 2px var(--bg2); }
  .swatch.on { border-color: var(--fg); }
  .preview { font-size: 13px; color: var(--muted); background: var(--bg3); border-radius: 8px; padding: 8px 10px; }
  .preview b { color: var(--fg); }
  .names { font-size: 12px; margin-top: 3px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  .del { margin-right: auto; }
  @media (max-width: 760px) { .row2 { grid-template-columns: 1fr; } }
</style>
