<script lang="ts">
  // The session header, two lines. First: the editable title (it shrinks last), status and a ⋯ menu
  // (schedules, jobs, link to task, + workspace, mark read/unread, raw, archive). Second, wrapping:
  // its workspaces (one `machine:path` pill with its availability and a ⚠ when another open session
  // shares the directory; several collapse into one chip whose popover lists and removes them),
  // badges, options, parent and task chips, and the usage statusline. With several
  // profiles the second line starts with the session's profile chip (it leads to the profile's ∗),
  // and ⋯ offers "Move to profile…".
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { profileLabel } from '../model/profiles';
  import { tasksFor } from '../model/projects';
  import { statusKind, statusLabel, statusTitle } from '../model/status';
  import { columnLabel } from '../model/tasks';
  import type { FeedSession, SessionDetail } from '../protocol';
  import EditableTitle from '../ui/EditableTitle.svelte';
  import ActionMenu, { type MenuAction } from '../ui/ActionMenu.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';
  import ProfileChip from '../profiles/ProfileChip.svelte';
  import AgentOptions from './AgentOptions.svelte';
  import SessionBadges from './SessionBadges.svelte';
  import Statusline from './Statusline.svelte';
  import Workspaces from './Workspaces.svelte';

  let { s, detail }: { s: FeedSession; detail: SessionDetail | null } = $props();
  const app = useApp();
  const tasks = $derived(tasksFor(app.ctxOf(s.profile_id), s));
  const profile = $derived(app.shownProfile(s.profile_id));
  const parent = $derived(s.parent_id ? app.client.feed.sessions.get(s.parent_id) : undefined);
  const key = $derived(s.id);
  const schedules = $derived(app.client.feed.scheduleList.filter((x) => x.session_id === s.id).length);
  const schedulesShown = $derived(app.view.panels['schedules:' + key] ?? schedules > 0);
  const online = $derived(app.client.online);
  const jobs = $derived(detail?.jobs.length ?? 0);
  const actions = $derived.by((): MenuAction[] => [
    { label: schedules ? `Schedules (${schedules})` : 'Schedules', checked: schedulesShown, run: () => (app.view.panels['schedules:' + key] = !schedulesShown) },
    ...(jobs ? [{ label: `Jobs (${jobs})`, aria: `Background jobs (${jobs})`, run: () => (app.view.modal = { kind: 'jobs', session: { id: s.id }, job: null }) }] : []),
    { label: tasks.length ? 'Tasks…' : 'Link to task…', run: () => (app.view.modal = { kind: 'link-tasks', session: { id: s.id } }) },
    ...(s.state !== 'closed'
      ? [{ label: '+ Workspace', aria: 'Add workspace', disabled: !online, run: () => (app.view.modal = { kind: 'add-workspace', session: { id: s.id } }) }]
      : []),
    s.unread
      ? { label: 'Mark read', disabled: !online, run: () => app.markRead(s) }
      : { label: 'Mark unread', disabled: !online, run: () => void app.client.call('session.mark_unread', onSession(s)) },
    { label: 'Raw transcript', run: () => (app.view.raw[key] = { from: null, to: null, item: null }) },
    ...(profile ? [{ label: 'Move to profile…', disabled: !online, run: () => app.moveToProfile({ from: profile.id, session_ids: [s.id] }) }] : []),
    s.state !== 'closed'
      ? { label: 'Archive', key: 'e', disabled: !online, run: () => void app.archive(s) }
      : { label: 'Unarchive', key: 'e', disabled: !online, run: () => void app.unarchive(s) },
  ]);
</script>

<div class="head" data-pane="header">
  <div class="title">
    <EditableTitle heading value={displayName(s)} editKey={key} onsave={(name) => app.client.call('session.update', { ...onSession(s), name })} />
    <span class="status" title={statusTitle(s, app.client.feed.machines)}><StatusIcon kind={statusKind(s)} /> {statusLabel(s)}</span>
    <span class="sp"></span>
    <ActionMenu label="Session actions" {actions} />
  </div>
  <div class="meta">
    {#if profile}{@const pf = profile}<ProfileChip profile={pf} title="{profileLabel(pf)}: all its sessions" onclick={() => app.openProject(null, pf.id)} />{/if}
    <Workspaces {s} />
    <SessionBadges {s} {detail} />
    <AgentOptions {s} />
    {#if parent}{@const p = parent}<button class="chip parent" title="Parent session: {displayName(p)}" onclick={() => app.open(p)}>↑ {displayName(p)}</button>{/if}
    {#each tasks as t (t.id)}<button class="chip task" title="Task: {t.title} ({columnLabel(t.status)})" aria-label="Task {t.title}" onclick={() => app.openTask(t.id)}><span aria-hidden="true">◆</span><span class="tt"> {t.title}</span></button>{/each}
    <Statusline {s} />
  </div>
</div>

<style>
  .head { flex: 1; min-width: 0; }
  /* The title shrinks last: status and ⋯ never wrap below it. */
  .title { display: flex; align-items: center; gap: 10px; min-width: 0; }
  .title > :global(.et) { flex: 0 1 auto; min-width: 0; }
  .status { flex: none; display: inline-flex; align-items: center; gap: 5px; font-size: 12.5px; white-space: nowrap; }
  .title > :global(.am) { flex: none; }
  .meta { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; margin-top: 6px; min-width: 0; }
  .chip { font: inherit; font-size: 12px; padding: 1px 8px; border-radius: 5px; background: var(--bg3); border: none; color: var(--fg); cursor: pointer; max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chip:hover { background: color-mix(in srgb, var(--accent) 15%, var(--bg3)); }
  .parent { color: var(--muted); }
  /* A phone shows a task as ◆ (its name in the tooltip). */
  @media (max-width: 760px) { .tt { display: none; } }
</style>
