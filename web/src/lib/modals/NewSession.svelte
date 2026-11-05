<script lang="ts">
  // New session: one or more workspaces (`machine:path`, the primary first; each machine picked
  // from the connected ones, its directory browsed with `fs.list` on that machine), name, tasks
  // (pre-filled when started from a task, incl. "+ New task …"), sandbox level (the defaults of
  // the machines and the tasks' projects, or an explicit choice), first message. A machine that
  // can't be reached is refused up front; a directory another open session uses is warned about.
  import { newOpId, useApp } from '../app/app.svelte';
  import type { NewSessionPreset } from '../app/view.svelte';
  import { RpcError, describeError } from '../api/errors';
  import { displayName } from '../model/filters';
  import { textContent } from '../model/misc';
  import { LEVELS, defaultLevel, levelLabel, looser } from '../model/sandbox';
  import { connKind, connLabel, isLive } from '../model/status';
  import { columnLabel } from '../model/tasks';
  import { normPath } from '../model/workspaces';
  import type { MachineId, SandboxLevel, SandboxProfile } from '../protocol';
  import Dropdown, { type Option } from '../ui/Dropdown.svelte';
  import Modal from '../ui/Modal.svelte';
  import PathCombo from '../ui/PathCombo.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';
  import TagInput, { type Suggestion } from '../ui/TagInput.svelte';

  let { preset }: { preset: NewSessionPreset } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);

  interface Row {
    /** Stable key for the list. */
    key: number;
    machine: MachineId;
    path: string;
  }
  let nextKey = 0;
  const row = (machine: MachineId, path: string): Row => ({ key: nextKey++, machine, path });
  const firstLive = () => feed.machineList.find(isLive)?.id ?? '';

  // svelte-ignore state_referenced_locally
  let rows = $state<Row[]>(preset.workspaces?.length ? preset.workspaces.map((w) => row(w.machine_id, w.path)) : [row(firstLive(), '')]);
  let name = $state('');
  // svelte-ignore state_referenced_locally
  let prompt = $state(preset.prompt ?? '');
  // svelte-ignore state_referenced_locally
  let tasks = $state<string[]>(preset.tasks?.slice() ?? []);
  let level = $state<SandboxLevel | 'default'>('default');
  let profiles = $state<SandboxProfile[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (app.client.online) void app.client.call('sandbox.profiles', {}).then((r) => (profiles = r?.profiles ?? []));
  });

  const machineIds = $derived([...new Set(rows.map((r) => r.machine).filter(Boolean))]);
  const taskProjects = $derived([...new Set(tasks.flatMap((id) => feed.tasks.get(id)?.project_ids ?? []))]);
  const defaults = $derived(defaultLevel(profiles, machineIds, taskProjects));
  const chosen = $derived(level === 'default' ? defaults : level);
  /** Machines that can't take a workspace now (the server refuses them). */
  const away = $derived(machineIds.map((id) => feed.machines.get(id)).filter((m) => !m || !isLive(m)));

  const machineOptions = $derived<Option[]>(feed.machineList.map((m) => ({ value: m.id, label: m.name, disabled: !isLive(m), hint: isLive(m) ? undefined : connLabel(m) })));
  const levelOptions = $derived<Option[]>([
    { value: 'default', label: `Defaults (${levelLabel(defaults)})` },
    ...LEVELS.map((l) => ({ value: l.value, label: l.label, hint: l.hint })),
  ]);

  /** Directories recently used on a machine, most recent first. */
  function recent(machine: MachineId): string[] {
    const user = feed.machines.get(machine)?.user ?? '';
    const dirs = app.ctx.sessions
      .flatMap((s) => s.workspaces.filter((w) => w.machine_id === machine).map((w) => ({ at: s.updated_at, dir: normPath(w.root ?? w.path, user) })))
      .sort((a, b) => b.at - a.at)
      .map((x) => x.dir);
    return [...new Set(dirs)].slice(0, 5);
  }

  /** Open sessions that already use a row's directory (sharing is allowed; edits may collide). */
  function sharing(r: Row): string[] {
    const user = feed.machines.get(r.machine)?.user ?? '';
    const want = normPath(r.path.trim().replace(/(.)\/+$/, '$1'), user);
    if (!want) return [];
    return app.ctx.sessions
      .filter((s) => s.state !== 'closed' && s.workspaces.some((w) => w.machine_id === r.machine && normPath(w.root ?? w.path, user) === want))
      .map(displayName);
  }

  function addRow() {
    const used = new Set(rows.map((r) => r.machine));
    rows = [...rows, row(feed.machineList.find((m) => isLive(m) && !used.has(m.id))?.id ?? firstLive(), '')];
  }
  const removeRow = (key: number) => (rows = rows.filter((r) => r.key !== key));
  function makePrimary(key: number) {
    const r = rows.find((x) => x.key === key);
    if (r) rows = [r, ...rows.filter((x) => x.key !== key)];
  }

  function suggestTasks(q: string, picked: string[]): Suggestion[] {
    const t = q.trim().toLowerCase();
    const order: Record<string, number> = { doing: 0, review: 1, todo: 2, backlog: 3 };
    const out: Suggestion[] = app.ctx.tasks
      .filter((x) => !picked.includes(x.id) && x.status !== 'done' && (!t || x.title.toLowerCase().includes(t)))
      .sort((a, b) => (order[a.status] ?? 4) - (order[b.status] ?? 4))
      .slice(0, 12)
      .map((x) => ({ value: x.id, label: x.title, hint: columnLabel(x.status) }));
    if (t && !app.ctx.tasks.some((x) => x.title.toLowerCase() === t)) out.push({ value: 'new:' + q.trim(), label: `+ New task “${q.trim()}”`, isNew: true });
    return out;
  }

  /** The reason the session can't be created yet, if any. */
  function problem(): string | null {
    if (!rows.length) return 'Add a workspace';
    if (rows.some((r) => !r.machine)) return 'Pick a machine for every workspace';
    if (away.length) return `${away.map((m) => (m ? `${m.name} is ${connLabel(m)}` : 'A machine was removed')).join('; ')}: a session can't start there`;
    if (rows.some((r) => !r.path.trim())) return 'Enter a directory for every workspace';
    return null;
  }

  /** A refusal in the user's terms: which machine couldn't be reached. */
  function refusal(e: unknown): string {
    if (e instanceof RpcError && e.data.kind === 'machine_unavailable') {
      const id = e.data.machine_id;
      return `${feed.machines.get(id)?.name ?? 'A machine'} can't be reached (${e.data.connectivity.state.replace(/_/g, ' ')}); the session wasn't created`;
    }
    return describeError(e);
  }

  async function create() {
    if (busy) return;
    error = problem();
    if (error) return;
    busy = true;
    try {
      const r = await app.client.request('session.create', {
        op_id: newOpId(),
        workspaces: rows.map((x) => ({ machine_id: x.machine, path: x.path.trim() })),
        name: name.trim() || null,
        sandbox_level: level === 'default' ? null : level,
        // The first message goes in the same call (the server overwrites its origin), and existing
        // tasks are linked (and moved to In progress) by the server.
        prompt: prompt.trim() ? { content: textContent(prompt.trim()), origin: { kind: 'user', device: app.client.deviceRef } } : null,
        task_ids: tasks.filter((v) => !v.startsWith('new:')),
      });
      const id = r.session.id;
      for (const v of tasks.filter((v) => v.startsWith('new:'))) {
        await app.client.call('tasks.create', { title: v.slice(4), status: 'doing', links: [id], project_ids: app.project ? [app.project.id] : [] });
      }
      close();
      app.open({ id });
    } catch (e) {
      error = refusal(e);
    } finally {
      busy = false;
    }
  }
</script>

{#snippet machineIcon(o: Option)}
  {@const m = feed.machines.get(o.value)}
  {#if m}<StatusIcon kind={connKind(m)} />{/if}
{/snippet}
{#snippet taskChip(v: string)}
  {#if v.startsWith('new:')}<span class="muted">new ·</span> {v.slice(4)}{:else}{feed.tasks.get(v)?.title ?? v}{/if}
{/snippet}

<Modal label="New session" oncancel={close} onprimary={create}>
  <h3>New session</h3>
  <div class="field" role="group" aria-label="Workspaces">
    <span>Workspaces</span>
    {#each rows as r, i (r.key)}
      {@const shared = sharing(r)}
      <div class="ws" data-workspace={i}>
        <Dropdown id="ns-machine-{i}" label="Machine {i + 1}" value={r.machine} options={machineOptions} icon={machineIcon} onchange={(v) => (r.machine = v)} />
        <span class="colon">:</span>
        <PathCombo id="ns-path-{i}" label="Directory {i + 1}" autofocus={i === 0} bind:value={r.path} machineId={r.machine} recent={recent(r.machine)} />
        {#if i === 0}
          <span class="prim" title="Relative paths and new shells default to the primary workspace">primary</span>
        {:else}
          <button class="btn xs ghost" title="Make this the primary workspace" onclick={() => makePrimary(r.key)}>↑ primary</button>
        {/if}
        {#if rows.length > 1}<button class="btn xs ghost" aria-label="Remove workspace {i + 1}" onclick={() => removeRow(r.key)}>✕</button>{/if}
      </div>
      {#if shared.length}<div class="small warn-t" data-overlap>⚠ Also used by {shared.join(', ')}: edits can collide</div>{/if}
    {/each}
    <button class="btn xs ghost add" onclick={addRow}>+ Workspace</button>
  </div>
  <label class="field"><span>Name</span><input placeholder="Optional" bind:value={name} /></label>
  <div class="field"><span>Tasks</span><TagInput label="Tasks" bind:values={tasks} suggest={suggestTasks} chip={taskChip} placeholder="Add to a task…" empty="Type to find a task" /></div>
  <div class="field">
    <span>Sandbox</span>
    <div class="row">
      <Dropdown label="Sandbox level" value={level} options={levelOptions} onchange={(v) => (level = v as SandboxLevel | 'default')} />
      {#if chosen === 'off'}<span class="badge off">no sandbox</span>
      {:else if level !== 'default' && looser(chosen, defaults)}<span class="badge">looser than defaults</span>{/if}
    </div>
  </div>
  <label class="field"><span>Prompt</span><textarea rows="3" placeholder="Optional" bind:value={prompt}></textarea></label>
  {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
  <div class="acts">
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={busy || !app.client.online} onclick={create}><span class="kbd">↵</span> Create</button>
  </div>
</Modal>

<style>
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .field > :global(.dd) { display: flex; }
  .ws { display: grid; grid-template-columns: minmax(110px, 0.35fr) auto minmax(0, 1fr) auto auto; align-items: center; gap: 6px; color: var(--fg); }
  .colon { font: 13px var(--mono); color: var(--muted); }
  .prim { font-size: 11px; color: var(--muted); padding: 0 6px; }
  .add { align-self: flex-start; }
  .row { display: flex; align-items: center; gap: 8px; }
  .badge { font-size: 11.5px; padding: 1px 8px; border-radius: 999px; border: 1px dashed var(--warn); color: var(--warn); }
  .badge.off { border: 1px solid var(--bad); color: var(--bad); font-weight: 650; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  @media (max-width: 760px) { .ws { grid-template-columns: 1fr auto; } .colon { display: none; } }
</style>
