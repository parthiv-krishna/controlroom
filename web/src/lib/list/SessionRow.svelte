<script lang="ts">
  // One session row: status icon and name; hovering shows a one-line preview and archive (or ↺
  // undo). Chips: context when filling up, todo progress, idle-with-jobs, subagent roll-up, fold.
  // Rows drag onto a task (card or group header) to link the session.
  import { useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { attentionOf, statusKind, statusTitle } from '../model/status';
  import type { FeedSession } from '../protocol';
  import ContextMeter, { ctxLevel } from '../ui/ContextMeter.svelte';
  import Icon from '../ui/Icon.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';

  let {
    s,
    indent = 0,
    child = false,
    kids = 0,
    folded = false,
    onfold,
    ancestor = false,
  }: { s: FeedSession; indent?: number; child?: boolean; kids?: number; folded?: boolean; onfold?: () => void; ancestor?: boolean } = $props();

  const app = useApp();
  const key = $derived(s.id);
  const on = $derived(!!app.view.current && app.view.current.id === key);
  const att = $derived(attentionOf(s));
  const ru = $derived(s.rollup);
  const idleJobs = $derived((s.state === 'idle' || s.state === 'dormant') && s.live_work.jobs > 0);

  function dragstart(e: DragEvent) {
    e.dataTransfer?.setData('application/x-cr-session', s.id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'link';
  }
  function onclick(e: MouseEvent) {
    if (!(e.target as HTMLElement).closest('button')) app.open(s);
  }
  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && e.target === e.currentTarget) app.open(s);
  }
</script>

<div
  class="row att-{att ?? 'none'}"
  class:on
  class:unread={s.unread}
  class:ancestor
  style:--indent={indent}
  role="option"
  tabindex="-1"
  aria-selected={on}
  title={ancestor ? "Parent of a match" : undefined}
  draggable="true"
  data-session={s.id}
  ondragstart={dragstart}
  {onclick}
  {onkeydown}
>
  <span class="st">{#if child}<span class="tree" aria-hidden="true">└</span>{/if}<StatusIcon kind={statusKind(s)} title={statusTitle(s, app.client.feed.machines)} /></span>
  <div class="main">
    <div class="name">
      <span class="nm">{displayName(s)}</span>
      {#if s.context && ctxLevel(s.context.used, s.context.size) !== 'ok'}<ContextMeter usage={s.context} compact />{/if}
      {#if s.todo}<span class="mini" title="Todo · auto-continue {s.todo.auto_continue}">{s.todo.done}/{s.todo.total}</span>{/if}
      {#if idleJobs}<span class="mini jobs" title="{s.live_work.jobs} jobs running">⚙ {s.live_work.jobs}</span>{/if}
      {#if ru.approvals}<span class="ru approvals" title="{ru.approvals} subagent(s) need you">↳ ⚑{ru.approvals}</span>
      {:else if ru.questions}<span class="ru questions" title="{ru.questions} subagent question(s)">↳ ?{ru.questions}</span>
      {:else if ru.problems}<span class="ru problems" title="{ru.problems} subagent(s) failed">↳ ✕{ru.problems}</span>{/if}
      {#if kids}<button class="fold" aria-expanded={!folded} aria-label="{folded ? 'Show' : 'Hide'} subagents" onclick={() => onfold?.()}>{folded ? '▸' : '▾'} {kids}</button>{/if}
    </div>
    <div class="prev">{s.preview}</div>
  </div>
  {#if s.state !== 'closed'}
    <button class="arch" title="Archive (e)" aria-label="Archive" onclick={() => app.archive(s)}><Icon name="archive" size={11} /></button>
  {:else}
    <button class="arch" title="Unarchive (e)" aria-label="Unarchive" onclick={() => app.unarchive(s)}><Icon name="unarchive" size={11} /></button>
  {/if}
</div>

<style>
  .row {
    position: relative; display: grid; grid-template-columns: 16px minmax(0, 1fr); gap: 0 8px; padding: 8px 10px;
    padding-left: calc(10px + var(--indent) * 16px); border-radius: 9px; cursor: pointer; outline: none;
  }
  .row:hover { background: var(--hover); }
  .row.on { background: var(--bg3); }
  .row:focus-visible { box-shadow: inset 0 0 0 1px var(--muted); }
  .row.ancestor { opacity: 0.5; }
  .st { display: grid; place-items: start center; padding-top: 3px; position: relative; }
  .tree { color: var(--muted); font-size: 11px; position: absolute; left: -14px; top: 1px; }
  .main { min-width: 0; }
  .name { display: flex; align-items: center; gap: 4px; font-weight: 450; }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .att-approval .name, .att-problem .name, .unread .name { font-weight: 600; }
  .prev { font-size: 12.5px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-height: 0; opacity: 0; transition: max-height 0.15s ease, opacity 0.15s ease; }
  .row:hover .prev, .row.on .prev { max-height: 20px; opacity: 1; margin-top: 2px; }
  .mini { font-size: 10.5px; font-weight: 600; font-variant-numeric: tabular-nums; padding: 0 5px; border-radius: 5px; background: var(--bg3); color: var(--muted); flex: none; }
  .mini.jobs { background: none; color: var(--info); border: 1px dashed color-mix(in srgb, var(--info) 50%, var(--line)); }
  .ru { font-size: 10.5px; font-weight: 650; padding: 0 5px; border-radius: 5px; flex: none; }
  .ru.approvals { color: #3a2a00; background: var(--warn); }
  .ru.questions { color: var(--warn); border: 1px solid var(--warn); }
  .ru.problems { color: var(--bad); border: 1px solid var(--bad); }
  .fold { font: inherit; font-size: 11px; color: var(--muted); background: none; border: 1px solid transparent; border-radius: 6px; padding: 0 5px; cursor: pointer; flex: none; }
  .fold:hover { border-color: var(--line); color: var(--fg); }
  .arch { position: absolute; right: 8px; top: 50%; transform: translateY(-50%); display: none; place-items: center; width: 26px; height: 26px; border: 1px solid var(--line); border-radius: 7px; background: var(--bg2); color: var(--muted); cursor: pointer; padding: 0; }
  .row:hover .arch { display: grid; }
  .arch:hover { color: var(--fg); }
  @media (hover: none) {
    .prev { max-height: 20px; opacity: 1; margin-top: 2px; }
    .row:hover .arch { display: none; }
  }
  @media (max-width: 760px) { .row { padding-top: 11px; padding-bottom: 11px; } }
</style>
