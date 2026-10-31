<script lang="ts">
  // New session: machine (offline ones disabled), agent (from agents.list, only usable ones),
  // working directory (browses the machine's directories), name, tasks (incl. "+ New task …"),
  // sandbox level (the defaults, or an explicit choice with the "looser than defaults" badge),
  // first message. Switching machine keeps everything typed.
  import { useApp } from '../app/app.svelte';
  import type { NewSessionPreset } from '../app/view.svelte';
  import { prefixMatch } from '../model/location';
  import { textContent } from '../model/misc';
  import { columnLabel, columnTasks, orderFor } from '../model/tasks';
  import { LEVELS, defaultLevel, levelLabel, looser } from '../model/sandbox';
  import { connKind, connLabel, isLive } from '../model/status';
  import type { AgentInfo, SandboxLevel, SandboxProfile } from '../protocol';
  import Dropdown, { type Option } from '../ui/Dropdown.svelte';
  import Modal from '../ui/Modal.svelte';
  import PathCombo from '../ui/PathCombo.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';
  import TagInput, { type Suggestion } from '../ui/TagInput.svelte';

  let { preset }: { preset: NewSessionPreset } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);

  // svelte-ignore state_referenced_locally
  let machineId = $state(preset.machine_id ?? feed.machineList.find(isLive)?.id ?? '');
  // svelte-ignore state_referenced_locally
  let cwd = $state(preset.cwd ?? '');
  let name = $state('');
  // svelte-ignore state_referenced_locally
  let prompt = $state(preset.prompt ?? '');
  // svelte-ignore state_referenced_locally
  let tasks = $state<string[]>(preset.tasks?.slice() ?? []);
  let level = $state<SandboxLevel | 'default'>('default');
  let agent = $state('');
  let agents = $state<AgentInfo[]>([]);
  let profiles = $state<SandboxProfile[]>([]);
  let busy = $state(false);

  const machine = $derived(feed.machines.get(machineId));
  $effect(() => {
    const id = machineId;
    agents = [];
    if (!machine || !isLive(machine) || !app.client.online) return;
    void app.client.call('agents.list', { machine_id: id }).then((r) => {
      if (!r || id !== machineId) return;
      agents = r.agents;
      if (!agents.some((a) => a.agent === agent && a.status === 'ok')) agent = agents.find((a) => a.status === 'ok')?.agent ?? '';
    });
  });
  $effect(() => {
    if (app.client.online) void app.client.call('sandbox.profiles', {}).then((r) => (profiles = r?.profiles ?? []));
  });

  const defaults = $derived(machine ? defaultLevel(profiles, machine, cwd.trim() || '~', feed.projectList) : 'strict');
  const chosen = $derived(level === 'default' ? defaults : level);
  const recent = $derived(
    [...new Set(app.ctx.sessions.filter((s) => s.machine_id === machineId).sort((a, b) => b.updated_at - a.updated_at).map((s) => s.cwd))].slice(0, 5),
  );
  const machineOptions = $derived<Option[]>(feed.machineList.map((m) => ({ value: m.id, label: m.name, disabled: !isLive(m), hint: isLive(m) ? undefined : connLabel(m) })));
  const levelOptions = $derived<Option[]>([
    { value: 'default', label: `Defaults (${levelLabel(defaults)})` },
    ...LEVELS.map((l) => ({ value: l.value, label: l.label, hint: l.hint })),
  ]);

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

  async function create() {
    if (busy) return;
    if (!machine || !isLive(machine)) return app.toasts.show('Pick a connected machine', 'error');
    if (!agent) return app.toasts.show('Pick an agent', 'error');
    if (!cwd.trim()) return app.toasts.show('Enter a working directory', 'error');
    busy = true;
    const r = await app.client.call('session.create', {
      machine_id: machineId,
      op_id: crypto.randomUUID(),
      agent,
      cwd: cwd.trim(),
      name: name.trim() || null,
      sandbox_level: level === 'default' ? null : level,
    });
    if (!r) return void (busy = false);
    const id = r.session.id;
    const ref = { machine_id: machineId, session_id: id };
    // The protocol has no atomic create-and-link: the first message and the task links follow.
    if (prompt.trim()) {
      await app.client.call('session.prompt', { machine_id: machineId, op_id: crypto.randomUUID(), id, content: textContent(prompt.trim()), origin: { kind: 'user', device: app.client.deviceRef }, if_busy: 'queue' });
    }
    for (const v of tasks) {
      if (v.startsWith('new:')) {
        await app.client.call('tasks.create', { title: v.slice(4), status: 'doing', links: [ref], project_ids: app.view.project ? [app.view.project] : [] });
        continue;
      }
      await app.client.call('tasks.link', { task_id: v, ...ref });
      const t = feed.tasks.get(v);
      if (t && (t.status === 'backlog' || t.status === 'todo')) await app.client.call('tasks.move', { task_id: v, status: 'doing', order: orderFor(columnTasks(app.ctx.tasks, 'doing'), v, null) });
    }
    busy = false;
    close();
    app.open({ machine_id: machineId, id });
  }
  // A project location the directory belongs to, shown as a hint.
  const inProjects = $derived(machine ? feed.projectList.filter((p) => p.location_prefixes.some((x) => prefixMatch({ cwd: cwd.trim() }, machine, x))).map((p) => p.name) : []);
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
  <div class="field"><span>Machine</span><Dropdown id="ns-machine" label="Machine" value={machineId} options={machineOptions} icon={machineIcon} onchange={(v) => (machineId = v)} /></div>
  <div class="field" role="radiogroup" aria-label="Agent">
    <span>Agent</span>
    <div class="agents">
      {#each agents as a (a.agent)}
        <label class="radio" class:off={a.status !== 'ok'} title={a.detail}><input type="radio" name="ns-agent" value={a.agent} bind:group={agent} disabled={a.status !== 'ok'} /> {a.agent}{#if a.status !== 'ok'}<span class="small muted">{a.status === 'missing' ? 'not installed' : "couldn't check"}</span>{/if}</label>
      {:else}
        <span class="small muted">{machine && isLive(machine) ? 'Loading agents…' : 'Pick a connected machine'}</span>
      {/each}
    </div>
  </div>
  <div class="field"><span>Working directory</span><PathCombo id="ns-cwd" bind:value={cwd} {machineId} {recent} />{#if inProjects.length}<span class="small muted">in {inProjects.join(', ')}</span>{/if}</div>
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
  <label class="field"><span>First message</span><textarea rows="3" placeholder="Optional" bind:value={prompt}></textarea></label>
  <div class="acts">
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={busy || !app.client.online} onclick={create}><span class="kbd">↵</span> Create session</button>
  </div>
</Modal>

<style>
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .field > :global(.dd) { display: flex; }
  .agents { display: flex; flex-wrap: wrap; gap: 6px 14px; color: var(--fg); }
  .radio { display: inline-flex; align-items: center; gap: 6px; }
  .radio.off { opacity: 0.6; }
  .row { display: flex; align-items: center; gap: 8px; }
  .badge { font-size: 11.5px; padding: 1px 8px; border-radius: 999px; border: 1px dashed var(--warn); color: var(--warn); }
  .badge.off { border: 1px solid var(--bad); color: var(--bad); font-weight: 650; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
</style>
