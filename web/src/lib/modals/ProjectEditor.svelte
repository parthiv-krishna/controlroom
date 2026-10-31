<script lang="ts">
  // A project view: name, color and location prefixes (`host:path`, `host:` for the whole machine,
  // `~/path` on any machine), with completion against each host's real directories and a preview
  // of what it matches.
  import { useApp } from '../app/app.svelte';
  import { parsePrefix, prefixText } from '../model/location';
  import { inProject, taskIn } from '../model/projects';
  import { displayName } from '../model/filters';
  import { isLive } from '../model/status';
  import type { LocationPrefix, Project } from '../protocol';
  import Modal from '../ui/Modal.svelte';
  import TagInput, { type Suggestion } from '../ui/TagInput.svelte';

  let { id, paths: presetPaths }: { id: string | null; paths?: string[] } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);
  const COLORS = ['#f97316', '#facc15', '#34d399', '#22d3ee', '#60a5fa', '#a78bfa', '#f472b6', '#f43f5e'];

  // svelte-ignore state_referenced_locally
  const base = id ? feed.projects.get(id) : undefined;
  let name = $state(base?.name ?? '');
  let color = $state(base?.color ?? COLORS[feed.projects.size % COLORS.length]);
  // svelte-ignore state_referenced_locally
  let paths = $state<string[]>(base?.location_prefixes.map((p) => prefixText(p, feed.machines)) ?? presetPaths?.slice() ?? []);
  const prefixes = $derived(paths.map((p) => parsePrefix(p, feed.machineList)).filter((p): p is LocationPrefix => p !== null));

  const preview = $derived.by(() => {
    const p: Project = { id: id ?? 'preview', name, color, location_prefixes: prefixes };
    return {
      sessions: app.ctx.sessions.filter((s) => s.state !== 'closed' && inProject(app.ctx, s, p)),
      tasks: app.ctx.tasks.filter((t) => t.status !== 'done' && taskIn(t, p)),
    };
  });

  async function suggest(q: string, picked: string[]): Promise<Suggestion[]> {
    q = q.trim();
    const colon = q.indexOf(':');
    if (colon < 0) {
      const hosts = feed.machineList.filter((m) => m.name.startsWith(q)).map((m) => ({ value: `${m.name}:`, label: `${m.name}:`, hint: 'whole machine', drill: true }));
      const any = /^[~/]/.test(q) ? [{ value: q, label: q, hint: 'any machine' }] : [];
      return [...any, ...hosts].filter((s) => !picked.includes(s.value));
    }
    const m = feed.machineList.find((x) => x.name === q.slice(0, colon));
    if (!m) return [{ value: q, label: `Unknown host ${q.slice(0, colon)}`, disabled: true }];
    const path = q.slice(colon + 1);
    const slash = path.lastIndexOf('/');
    const dir = path.endsWith('/') ? path.slice(0, -1) || '/' : slash < 0 ? '' : slash === 0 ? '/' : path.slice(0, slash);
    const part = path.endsWith('/') ? '' : path.slice(slash + 1);
    const out: Suggestion[] = [];
    if (path && !path.endsWith('/')) out.push({ value: `${m.name}:${path}`, label: path, dim: `${m.name}:`, hint: 'prefix' });
    if (!dir) {
      for (const root of ['~', '/']) if (root.startsWith(part)) out.push({ value: `${m.name}:${root}`, label: root, dim: `${m.name}:`, drill: true });
    } else if (isLive(m)) {
      const r = await app.client.call('fs.list', { machine_id: m.id, path: dir });
      for (const e of r?.entries ?? []) {
        if (e.kind !== 'dir' || !e.name.startsWith(part)) continue;
        const p = dir === '/' ? `/${e.name}` : `${dir}/${e.name}`;
        out.push({ value: `${m.name}:${p}`, label: p, dim: `${m.name}:`, drill: true });
      }
    }
    return out.filter((s) => !picked.includes(s.value)).slice(0, 40);
  }

  async function save() {
    const unknown = paths.filter((p) => !parsePrefix(p, feed.machineList));
    if (unknown.length) return app.toasts.show(`Unknown host in ${unknown.join(', ')}`, 'error');
    const fields = { name: name.trim() || 'Untitled', color, location_prefixes: prefixes };
    const p = id ? await app.client.call('projects.update', { project_id: id, ...fields }) : await app.client.call('projects.create', fields);
    if (!p) return;
    close();
    app.openProject(p.id);
  }
  async function remove() {
    if (!id || !(await app.dialogs.confirm({ title: 'Delete project view?', body: 'Sessions and tasks are not affected.', ok: 'Delete', danger: true }))) return;
    if (await app.client.call('projects.delete', { project_id: id })) {
      close();
      app.openProject(null);
    }
  }
</script>

{#snippet pathChip(v: string)}
  {#if v.includes(':')}<b>{v.slice(0, v.indexOf(':'))}</b>:{v.slice(v.indexOf(':') + 1) || '*'}{:else}<span class="muted">*:</span>{v}{/if}
{/snippet}

<Modal label="Project view" oncancel={close} onprimary={save}>
  <h3>{id ? 'Edit' : 'New'} project view</h3>
  <div class="row2">
    <label class="field"><span>Name</span><input bind:value={name} data-autofocus /></label>
    <div class="field">
      <span>Color</span>
      <div class="swatches" role="radiogroup" aria-label="Color">
        {#each COLORS as c (c)}<button class="swatch" class:on={color === c} style:--sw={c} role="radio" aria-checked={color === c} aria-label={c} onclick={() => (color = c)}></button>{/each}
      </div>
    </div>
  </div>
  <div class="field"><span>Locations</span><TagInput label="Locations" mono bind:values={paths} {suggest} chip={pathChip} placeholder="host:path or ~/path" empty="Type a host or path" /></div>
  <div class="preview">
    Matches <b>{preview.sessions.length}</b> session{preview.sessions.length === 1 ? '' : 's'} and <b>{preview.tasks.length}</b> open task{preview.tasks.length === 1 ? '' : 's'}
    {#if preview.sessions.length}<div class="names">{preview.sessions.slice(0, 6).map(displayName).join(' · ')}{preview.sessions.length > 6 ? ` · +${preview.sessions.length - 6} more` : ''}</div>{/if}
  </div>
  <div class="acts">
    {#if id}<button class="btn ghost bad-t del" onclick={remove}>Delete view</button>{/if}
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
