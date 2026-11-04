<script lang="ts">
  // The session header: editable title, status, its workspaces (`machine:path` pills with each
  // machine's availability, and a warning when another open session shares a directory), options
  // and context, badges, icon actions (schedules, mark read/unread, archive), task chips and
  // "link to task".
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { tasksFor } from '../model/projects';
  import { statusKind, statusLabel, statusTitle } from '../model/status';
  import { columnLabel } from '../model/tasks';
  import type { FeedSession, SessionDetail } from '../protocol';
  import EditableTitle from '../ui/EditableTitle.svelte';
  import Icon from '../ui/Icon.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';
  import AgentOptions from './AgentOptions.svelte';
  import SessionBadges from './SessionBadges.svelte';
  import Workspaces from './Workspaces.svelte';

  let { s, detail }: { s: FeedSession; detail: SessionDetail | null } = $props();
  const app = useApp();
  const tasks = $derived(tasksFor(app.ctx, s));
  const parent = $derived(s.parent_id ? app.client.feed.sessions.get(s.parent_id) : undefined);
  const key = $derived(s.id);
  const schedules = $derived(app.client.feed.scheduleList.filter((x) => x.session_id === s.id).length);
  const schedulesShown = $derived(app.view.panels['schedules:' + key] ?? schedules > 0);
  const online = $derived(app.client.online);
</script>

<div class="head" data-pane="header">
  <div class="title">
    <EditableTitle heading value={displayName(s)} editKey={key} onsave={(name) => app.client.call('session.update', { ...onSession(s), name })} />
    <span class="status" title={statusTitle(s, app.client.feed.machines)}><StatusIcon kind={statusKind(s)} /> {statusLabel(s)}</span>
    {#if parent}{@const p = parent}<button class="parent" title="Open the parent session" onclick={() => app.open(p)}>↑ subagent of {displayName(p)}</button>{/if}
  </div>
  <div class="meta">
    <Workspaces {s} />
    <SessionBadges {s} {detail} />
    <AgentOptions {s} />
    <span class="acts">
      <button class="sa" class:on={schedulesShown} title="Schedules" aria-label="Schedules" aria-pressed={schedulesShown} onclick={() => (app.view.panels['schedules:' + key] = !schedulesShown)}><Icon name="clock" />{#if schedules}<span class="n">{schedules}</span>{/if}</button>
      {#if s.unread}
        <button class="sa" title="Mark read" aria-label="Mark read" disabled={!online} onclick={() => app.markRead(s)}><Icon name="read" /></button>
      {:else}
        <button class="sa" title="Mark unread" aria-label="Mark unread" disabled={!online} onclick={() => app.client.call('session.mark_unread', onSession(s))}><Icon name="unread" /></button>
      {/if}
      {#if s.state !== 'closed'}
        <button class="sa" title="Archive (e)" aria-label="Archive" disabled={!online} onclick={() => app.archive(s)}><Icon name="archive" size={13} /></button>
      {:else}
        <button class="sa" title="Unarchive (e)" aria-label="Unarchive" disabled={!online} onclick={() => app.unarchive(s)}><Icon name="unarchive" /></button>
      {/if}
    </span>
    <span class="tasks">
      {#each tasks as t (t.id)}<button class="task" title="Task · {columnLabel(t.status)}" onclick={() => app.openTask(t.id)}>◆ {t.title}</button>{/each}
      <button class="btn xs ghost" title="Link to a task" onclick={() => (app.view.modal = { kind: 'link-tasks', session: { id: s.id } })}>{tasks.length ? '+' : '◆ link to task'}</button>
    </span>
  </div>
</div>

<style>
  .head { flex: 1; min-width: 0; }
  .title { display: flex; align-items: center; gap: 10px; min-width: 0; flex-wrap: wrap; }
  .status { display: inline-flex; align-items: center; gap: 5px; font-size: 12.5px; white-space: nowrap; }
  .parent { font: inherit; font-size: 11.5px; padding: 1px 8px; border-radius: 999px; border: 1px solid var(--line); background: none; color: var(--muted); cursor: pointer; }
  .meta { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; margin-top: 8px; }
  .acts { display: inline-flex; align-items: center; gap: 2px; }
  .sa { position: relative; width: 26px; height: 26px; display: grid; place-items: center; background: none; border: 1px solid transparent; border-radius: 7px; color: var(--muted); cursor: pointer; padding: 0; }
  .sa:hover:not(:disabled) { color: var(--fg); background: var(--bg3); border-color: var(--line); }
  .sa.on { color: var(--fg); }
  .sa:disabled { opacity: 0.4; }
  .n { position: absolute; top: -3px; right: -3px; font-size: 9.5px; font-weight: 700; background: var(--bg3); border-radius: 999px; padding: 0 4px; color: var(--fg); }
  .tasks { display: inline-flex; gap: 4px; flex-wrap: wrap; align-items: center; }
  .task { font: inherit; font-size: 12px; padding: 1px 8px; border-radius: 5px; background: var(--bg3); border: none; color: var(--fg); cursor: pointer; max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .task:hover { background: color-mix(in srgb, var(--accent) 15%, var(--bg3)); }
</style>
