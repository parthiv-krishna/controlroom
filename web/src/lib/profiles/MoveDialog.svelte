<script lang="ts">
  // Move to profile: machine entries (moved, or shared with `share`), projects with their tasks,
  // and sessions, in one `profiles.move`. Every change previews the move (`dry_run`): what moves
  // along, the links and grants that would cross profiles (removed), and the open sessions left
  // with a workspace on a machine that leaves. Live agents of moved sessions restart.
  import { useApp } from '../app/app.svelte';
  import type { MovePreset } from '../app/view.svelte';
  import { describeError } from '../api/errors';
  import { displayName } from '../model/filters';
  import type { FeedSession, MachineId, ProfilesMoveResult, ProjectId, SessionId } from '../protocol';
  import Dropdown, { type Option } from '../ui/Dropdown.svelte';
  import Modal from '../ui/Modal.svelte';

  let { preset }: { preset: MovePreset } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);

  // svelte-ignore state_referenced_locally
  let from = $state(preset.from);
  // svelte-ignore state_referenced_locally
  let to = $state(preset.to ?? app.profiles.find((p) => p.id !== preset.from)?.id ?? '');
  // svelte-ignore state_referenced_locally
  let machines = $state<MachineId[]>(preset.machine_ids?.slice() ?? []);
  // svelte-ignore state_referenced_locally
  let projects = $state<ProjectId[]>(preset.project_ids?.slice() ?? []);
  // svelte-ignore state_referenced_locally
  let sessions = $state<SessionId[]>(preset.session_ids?.slice() ?? []);
  // svelte-ignore state_referenced_locally
  let share = $state(preset.share ?? false);
  let preview = $state<ProfilesMoveResult | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  const source = $derived(app.ctxOf(from));
  const target = $derived(app.profiles.find((p) => p.id === to) ?? null);
  /** Open sessions, plus archived ones already picked. */
  const sessionChoices = $derived(source.sessions.filter((s) => s.state !== 'closed' || sessions.includes(s.id)).sort((a, b) => displayName(a).localeCompare(displayName(b))));
  /** Sharing is for non-private profiles only. */
  const canShare = $derived(!!target && !target.private && !app.profiles.find((p) => p.id === from)?.private);
  const empty = $derived(!machines.length && !projects.length && !sessions.length);
  const params = $derived({
    profile_id: to,
    machine_ids: machines,
    project_ids: projects,
    session_ids: sessions,
    share: share && canShare && machines.length > 0,
  });
  const profileOptions = (except: string): Option[] => app.profiles.filter((p) => p.id !== except).map((p) => ({ value: p.id, label: p.name, hint: p.private ? 'private' : undefined }));

  // The preview follows every change; a slower, older answer never replaces a newer one.
  let asked = 0;
  $effect(() => {
    const p = params;
    preview = null;
    error = null;
    if (empty || !to || !app.client.online) return;
    const n = ++asked;
    app.client.request('profiles.move', { ...p, dry_run: true }).then(
      (r) => n === asked && (preview = r),
      (e) => n === asked && (error = describeError(e)),
    );
  });

  function toggle<T>(list: T[], v: T, on: boolean): T[] {
    return on ? [...list, v] : list.filter((x) => x !== v);
  }
  function setFrom(id: string) {
    from = id;
    machines = [];
    projects = [];
    sessions = [];
    if (to === id) to = app.profiles.find((p) => p.id !== id)?.id ?? '';
  }

  const nameOf = (id: SessionId) => {
    const s = feed.sessions.get(id);
    return s ? displayName(s) : 'missing session';
  };
  const live = (id: SessionId) => {
    const s: FeedSession | undefined = feed.sessions.get(id);
    return !!s && s.state !== 'closed' && s.state !== 'dormant';
  };
  const restarts = $derived(preview?.session_ids.filter(live) ?? []);

  async function move() {
    if (busy || empty || !target) return;
    busy = true;
    error = null;
    try {
      await app.client.request('profiles.move', params);
      close();
      app.toasts.show(`Moved to ${target.name}`, 'ok');
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal label="Move to profile" oncancel={close} onprimary={move}>
  <h3>Move to profile</h3>
  <div class="row2">
    <div class="field"><span>From</span><Dropdown label="From profile" value={from} options={app.profiles.map((p) => ({ value: p.id, label: p.name }))} onchange={setFrom} /></div>
    <div class="field"><span>To</span><Dropdown label="To profile" value={to} options={profileOptions(from)} onchange={(v) => (to = v)} /></div>
  </div>
  <div class="pick">
    {#if source.machineList.length}
      <div class="sec-h">Machines</div>
      {#each source.machineList as m (m.id)}
        <label class="check"><input type="checkbox" checked={machines.includes(m.id)} onchange={(e) => (machines = toggle(machines, m.id, e.currentTarget.checked))} /> {m.name}</label>
      {/each}
      {#if machines.length}
        <label class="check sub" title={canShare ? 'Add the target to their profiles instead of replacing them' : 'A private profile’s machines aren’t shared'}>
          <input type="checkbox" bind:checked={share} disabled={!canShare} /> Share instead of moving
        </label>
      {/if}
    {/if}
    {#if source.projects.length}
      <div class="sec-h">Projects</div>
      {#each source.projects as p (p.id)}
        <label class="check"><input type="checkbox" checked={projects.includes(p.id)} onchange={(e) => (projects = toggle(projects, p.id, e.currentTarget.checked))} /> <span class="pdot" style:background={p.color}></span>{p.name}</label>
      {/each}
    {/if}
    {#if sessionChoices.length}
      <div class="sec-h">Sessions</div>
      {#each sessionChoices as s (s.id)}
        <label class="check"><input type="checkbox" checked={sessions.includes(s.id)} onchange={(e) => (sessions = toggle(sessions, s.id, e.currentTarget.checked))} /> {displayName(s)}</label>
      {/each}
    {/if}
  </div>
  {#if preview}
    {@const p = preview}
    <div class="preview" data-preview aria-label="What moves" role="region">
      {#if p.task_ids.length}<div><b>Tasks</b> {p.task_ids.map((id) => feed.tasks.get(id)?.title ?? 'missing task').join(', ')}</div>{/if}
      {#if p.session_ids.length}<div><b>Sessions</b> {p.session_ids.map(nameOf).join(', ')}</div>{/if}
      {#if restarts.length}<div class="warn-t" role="alert">Live agents restart: {restarts.map(nameOf).join(', ')}</div>{/if}
      {#if p.unlinked.length}<div><b>Unlinked</b> {p.unlinked.map((l) => `${feed.tasks.get(l.task_id)?.title ?? 'missing task'} and ${nameOf(l.session_id)}`).join('; ')}</div>{/if}
      {#if p.revoked_grants.length}<div><b>Contact grants revoked</b> {p.revoked_grants.length}</div>{/if}
      {#if p.stranded.length}<div class="warn-t" title="Their workspace on a machine that leaves becomes unavailable"><b>Left behind</b> {p.stranded.map(nameOf).join(', ')}</div>{/if}
      {#if !p.task_ids.length && !p.session_ids.length && !p.unlinked.length && !p.revoked_grants.length && !p.stranded.length}<div class="muted">Nothing else changes</div>{/if}
    </div>
  {/if}
  {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
  <div class="acts">
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={busy || empty || !target || !app.client.online} onclick={move}><span class="kbd">↵</span> {share && canShare && machines.length ? 'Share' : 'Move'}</button>
  </div>
</Modal>

<style>
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .field > :global(.dd) { display: flex; }
  .row2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .pick { display: flex; flex-direction: column; max-height: 40vh; overflow-y: auto; }
  .check { display: flex; align-items: center; gap: 8px; padding: 3px 0; font-size: 13.5px; }
  .check.sub { padding-left: 24px; color: var(--muted); }
  .pdot { width: 8px; height: 8px; border-radius: 2px; }
  .preview { font-size: 13px; background: var(--bg3); border-radius: 8px; padding: 8px 10px; display: flex; flex-direction: column; gap: 4px; overflow-wrap: anywhere; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  @media (max-width: 760px) { .row2 { grid-template-columns: 1fr; } }
</style>
