<script lang="ts">
  // A task card: project badge, priority, title, checklist count, linked sessions (status icons),
  // roll-up counts and a suggested column move (never applied automatically). Cards drag between
  // columns; a session row dropped on a card links the session.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { keyOf } from '../model/projects';
  import { statusKind } from '../model/status';
  import { columnLabel, rollup, suggestMove } from '../model/tasks';
  import type { Task } from '../protocol';
  import StatusIcon from '../ui/StatusIcon.svelte';
  import ProjectBadge from './ProjectBadge.svelte';

  let { t, onmove }: { t: Task; onmove: (status: Task['status']) => void } = $props();
  const app = useApp();
  const r = $derived(rollup(app.ctx, t));
  const suggestion = $derived(suggestMove(app.ctx, t));
  const done = $derived(t.checklist.filter((c) => c.done).length);
  const shown = $derived(r.sessions.filter((s) => !s || s.state !== 'closed'));
  let dismissed = $state<string | null>(null);
  let over = $state(false);

  function dragstart(e: DragEvent) {
    e.dataTransfer?.setData('application/x-cr-task', t.id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }
  function dragover(e: DragEvent) {
    if (!e.dataTransfer?.types.includes('application/x-cr-session')) return;
    e.preventDefault();
    e.stopPropagation();
    over = true;
  }
  function drop(e: DragEvent) {
    over = false;
    const raw = e.dataTransfer?.getData('application/x-cr-session');
    if (!raw) return;
    e.preventDefault();
    e.stopPropagation();
    const link = JSON.parse(raw) as { machine_id: string; session_id: string };
    void app.client.call('tasks.link', { task_id: t.id, ...link });
  }
  function open(e: MouseEvent) {
    if (!(e.target as HTMLElement).closest('button')) app.openTask(t.id);
  }
</script>

<div
  class="card"
  class:hot={r.approvals > 0}
  class:bad={!r.approvals && r.problems > 0}
  class:on={app.view.openTask === t.id}
  class:done={t.status === 'done'}
  class:over
  role="button"
  tabindex="0"
  draggable="true"
  data-task={t.id}
  aria-label={t.title}
  ondragstart={dragstart}
  ondragover={dragover}
  ondragleave={() => (over = false)}
  ondrop={drop}
  onclick={open}
  onkeydown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) app.openTask(t.id); }}
>
  <div class="top">
    <ProjectBadge {t} /><span class="prio {t.priority}">{t.priority.toUpperCase()}</span><b>{t.title}</b>
    {#if t.checklist.length}<span class="check" class:all={done === t.checklist.length} title="Checklist">{done}/{t.checklist.length}</span>{/if}
  </div>
  {#if shown.length}
    <div class="ss">
      {#each shown.slice(0, 4) as s, i (s ? keyOf(s) : `missing-${i}`)}
        {#if s}
          <button class="s" class:unread={s.unread} title={displayName(s)} onclick={() => app.open(s)}><StatusIcon kind={statusKind(s)} /><span class="sn">{displayName(s)}</span>{#if s.todo}<span class="todo">{s.todo.done}/{s.todo.total}</span>{/if}</button>
        {:else}
          <span class="s missing" title="Deleted, or its machine was removed"><span class="q">?</span><span class="sn">missing session</span></span>
        {/if}
      {/each}
      {#if shown.length > 4}<div class="more">+{shown.length - 4} more</div>{/if}
    </div>
  {/if}
  <div class="foot">
    {#if r.approvals}<span class="rp" title="need you"><StatusIcon kind="attention" />{r.approvals}</span>{/if}
    {#if r.questions}<span class="rp" title="questions"><StatusIcon kind="asking" />{r.questions}</span>{/if}
    {#if r.problems}<span class="rp" title="failed or stopped"><StatusIcon kind="stopped" />{r.problems}</span>{/if}
    {#if r.working}<span class="rp" title="working"><StatusIcon kind="working" />{r.working}</span>{/if}
    {#if r.unread}<span class="rp" title="finished, unread"><StatusIcon kind="complete" />{r.unread}</span>{/if}
    {#if r.stale}<span class="rp" title="disconnected"><StatusIcon kind="offline" />{r.stale}</span>{/if}
    {#if !t.links.length}<span class="rp none">not started</span>{/if}
    {#if !t.links.length && t.status !== 'done'}<button class="btn xs ghost start" title="Start a session for this task" onclick={() => app.startTask(t)}>▶ start</button>{/if}
  </div>
  {#if suggestion && dismissed !== suggestion.to}
    {@const to = suggestion.to}
    <div class="sug">
      <span title={suggestion.why}>→ {columnLabel(to)}?</span>
      <button class="btn xs" disabled={!app.client.online} onclick={() => onmove(to)}>Move</button>
      <button class="btn xs ghost" aria-label="Dismiss suggestion" onclick={() => (dismissed = to)}>✕</button>
    </div>
  {/if}
</div>

<style>
  .card { background: var(--bg2); border: 1px solid var(--line); border-radius: 9px; padding: 8px 9px; display: flex; flex-direction: column; gap: 6px; cursor: pointer; font-size: 13px; }
  .card:hover { border-color: color-mix(in srgb, var(--accent) 40%, var(--line)); }
  .card.on { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .card.hot { border-left: 3px solid var(--warn); }
  .card.bad { border-left: 3px solid var(--bad); }
  .card.over { outline: 2px dashed var(--accent); outline-offset: 2px; }
  .card.done b { color: var(--muted); text-decoration: line-through; }
  .top { display: flex; gap: 6px; align-items: flex-start; }
  .top b { flex: 1; min-width: 0; font-weight: 600; line-height: 1.3; }
  .check { flex: none; font-size: 11.5px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .check.all { color: var(--ok); }
  .ss { display: flex; flex-direction: column; margin: 0 -6px; }
  .s { display: flex; gap: 8px; align-items: center; font: inherit; font-size: 12.5px; color: inherit; background: none; border: none; width: 100%; text-align: left; padding: 4px 6px; border-radius: 7px; cursor: pointer; }
  .s:hover { background: var(--hover); }
  .s.unread .sn { font-weight: 600; }
  .s.missing { color: var(--muted); font-style: italic; cursor: default; }
  .q { width: 14px; text-align: center; }
  .sn { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .todo { font-size: 10.5px; color: var(--muted); }
  .more { font-size: 11px; color: var(--muted); padding-left: 28px; }
  .foot { display: flex; gap: 5px; align-items: center; flex-wrap: wrap; }
  .rp { display: inline-flex; align-items: center; gap: 3px; font-size: 11px; padding: 0 6px; border-radius: 999px; background: var(--bg3); }
  .rp.none { font-style: italic; color: var(--muted); }
  .start { margin-left: auto; }
  .sug { font-size: 11.5px; background: color-mix(in srgb, var(--info) 10%, var(--bg3)); padding: 4px 6px; border-radius: 6px; display: flex; gap: 4px; align-items: center; }
</style>
