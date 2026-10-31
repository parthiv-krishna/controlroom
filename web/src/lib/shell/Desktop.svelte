<script lang="ts">
  // Desktop (> 760px): three edge panes around the conversation: sessions ← (rail + list), tasks ↑,
  // notifications →. Each pane is pinned (resizable by dragging its inner edge) or unpinned: hovering
  // its pull tab or the screen edge peeks it over the content, and the tab becomes the pin.
  import { useApp } from '../app/app.svelte';
  import SessionList from '../list/SessionList.svelte';
  import { health } from '../model/projects';
  import NotificationsPanel from '../notifications/NotificationsPanel.svelte';
  import SessionHeader from '../session/SessionHeader.svelte';
  import SessionPane from '../session/SessionPane.svelte';
  import TasksPane from '../tasks/TasksPane.svelte';
  import Icon from '../ui/Icon.svelte';
  import Stoplight from '../ui/Stoplight.svelte';
  import MachineStatus from './MachineStatus.svelte';
  import Rail from './Rail.svelte';

  const app = useApp();
  const { view, prefs } = app;
  const cur = $derived(app.current);
  const detail = $derived(cur ? app.detail(cur) : null);
  const h = $derived(health(app.ctx, app.project));
  const notesShown = $derived(prefs.notesPin || view.notesOpen);

  // ---- peek on hover (unpinned panes only)
  const timers: Partial<Record<'l' | 't' | 'r', ReturnType<typeof setTimeout>>> = {};
  function edge(side: 'l' | 't' | 'r', trigger: boolean, open: boolean, set: (v: boolean) => void, inReach: boolean) {
    if (!open) {
      if (trigger) set(true);
      return;
    }
    if (inReach) {
      clearTimeout(timers[side]);
      delete timers[side];
    } else if (!timers[side]) {
      timers[side] = setTimeout(() => {
        delete timers[side];
        set(false);
      }, 300);
    }
  }
  function onmove(e: MouseEvent) {
    if (resizing) return drag(e);
    if (view.modal || view.settings || view.menu || app.dialogs.current) return;
    const x = e.clientX;
    const y = e.clientY;
    const w = window.innerWidth;
    const onTab = (side: string) => !!(e.target as HTMLElement).closest?.(`.tab.${side}`);
    const railW = 74;
    if (!prefs.leftPin) edge('l', x <= 6 || onTab('l'), view.leftPeek, (v) => (view.leftPeek = v), x <= railW + prefs.listWidth + 24);
    const colL = prefs.leftPin ? railW + prefs.listWidth : 0;
    const colR = w - (prefs.notesPin ? prefs.notesWidth : 0);
    if (!prefs.topPin) edge('t', (y <= 4 && Math.abs(x - (colL + colR) / 2) <= 70) || onTab('t'), view.topPeek, (v) => (view.topPeek = v), y <= window.innerHeight * 0.56 + 24);
    if (!prefs.notesPin) edge('r', x >= w - 6 || onTab('r'), view.notesOpen, (v) => (view.notesOpen = v), x >= w - prefs.notesWidth - 24);
  }

  // ---- drag to resize (pinned panes)
  let resizing: { k: 'list' | 'notes' | 'board'; x: number; y: number; start: number } | null = null;
  function startResize(e: MouseEvent, k: 'list' | 'notes' | 'board') {
    e.preventDefault();
    const start = k === 'list' ? prefs.listWidth : k === 'notes' ? prefs.notesWidth : prefs.boardHeight || Math.round(window.innerHeight * 0.44);
    resizing = { k, x: e.clientX, y: e.clientY, start };
    document.body.classList.add('resizing');
    if (k === 'board') document.body.classList.add('rows');
  }
  function drag(e: MouseEvent) {
    if (!resizing) return;
    const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, Math.round(v)));
    if (resizing.k === 'list') prefs.listWidth = clamp(resizing.start + e.clientX - resizing.x, 240, 640);
    else if (resizing.k === 'notes') prefs.notesWidth = clamp(resizing.start - (e.clientX - resizing.x), 280, 620);
    else prefs.boardHeight = clamp(resizing.start + e.clientY - resizing.y, 160, window.innerHeight * 0.75);
  }
  function endResize() {
    if (!resizing) return;
    resizing = null;
    document.body.classList.remove('resizing', 'rows');
  }
</script>

<svelte:window onmousemove={onmove} onmouseup={endResize} />

<div
  class="cx"
  class:left-unpinned={!prefs.leftPin}
  class:left-peek={view.leftPeek}
  class:top-pinned={prefs.topPin}
  class:notes-pinned={prefs.notesPin}
  style:--list-w="{prefs.listWidth}px"
  style:--notes-w="{prefs.notesWidth}px"
  style:--board-h={prefs.boardHeight ? `${prefs.boardHeight}px` : '44vh'}
>
  <Rail />
  <SessionList />
  <header class="top">
    {#if cur}<SessionHeader s={cur} {detail} />{:else}<span class="sp"></span>{/if}
    <MachineStatus />
  </header>
  {#if prefs.topPin || view.topPeek}
    <section class="boardp" class:pinned={prefs.topPin} class:overlay={!prefs.topPin} aria-label="Tasks">
      <div class="bph"><b>Tasks</b></div>
      <TasksPane />
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions (a mouse drag handle; [ t ] toggle the panes by keyboard) -->
      {#if prefs.topPin}<div class="rs h" role="separator" aria-orientation="horizontal" aria-label="Resize tasks" onmousedown={(e) => startResize(e, 'board')}></div>{/if}
    </section>
  {/if}
  {#if notesShown}<NotificationsPanel overlay={!prefs.notesPin} />{/if}
  <main class="main">
    {#if cur}<SessionPane s={cur} />{:else}<div class="empty">Select a session</div>{/if}
  </main>

  {#if !prefs.leftPin && !view.leftPeek}
    <div class="tab l" title="Sessions ([)" aria-hidden="true"><Icon name="list" size={15} /></div>
  {:else}
    <button class="tab pin l" class:on={prefs.leftPin} title="{prefs.leftPin ? 'Unpin' : 'Pin'} sessions ([)" aria-label="{prefs.leftPin ? 'Unpin' : 'Pin'} sessions" onclick={() => app.toggleLeft()}><Icon name={prefs.leftPin ? 'pinned' : 'pin'} /></button>
  {/if}
  {#if !prefs.topPin && !view.topPeek}
    <div class="tab t" title="Tasks (t)" aria-hidden="true"><Icon name="board" size={15} /></div>
  {:else}
    <button class="tab pin t" class:on={prefs.topPin} title="{prefs.topPin ? 'Unpin' : 'Pin'} tasks (t)" aria-label="{prefs.topPin ? 'Unpin' : 'Pin'} tasks" onclick={() => app.toggleTop()}><Icon name={prefs.topPin ? 'pinned' : 'pin'} /></button>
  {/if}
  {#if !notesShown}
    <div class="tab r" title="Notifications (])" aria-hidden="true"><Icon name="bell" size={15} /><Stoplight {h} /></div>
  {:else}
    <button class="tab pin r" class:on={prefs.notesPin} title="{prefs.notesPin ? 'Unpin' : 'Pin'} notifications (])" aria-label="{prefs.notesPin ? 'Unpin' : 'Pin'} notifications" onclick={() => app.toggleNotes()}><Icon name={prefs.notesPin ? 'pinned' : 'pin'} /></button>
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions (mouse drag handles) -->
  {#if prefs.leftPin}<div class="rs v list" role="separator" aria-orientation="vertical" aria-label="Resize sessions" onmousedown={(e) => startResize(e, 'list')}></div>{/if}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  {#if prefs.notesPin}<div class="rs v notes" role="separator" aria-orientation="vertical" aria-label="Resize notifications" onmousedown={(e) => startResize(e, 'notes')}></div>{/if}
</div>

<style>
  .cx {
    --c-rail: var(--rail-w); --c-list: var(--list-w); --c-notes: 0px; --r-board: 0px;
    display: grid; height: 100%;
    grid-template-columns: var(--c-rail) var(--c-list) minmax(0, 1fr) var(--c-notes);
    grid-template-rows: var(--r-board) auto minmax(0, 1fr);
    grid-template-areas: 'rail list board notes' 'rail list top notes' 'rail list main notes';
  }
  .cx.left-unpinned { --c-rail: 0px; --c-list: 0px; }
  .cx.notes-pinned { --c-notes: var(--notes-w); }
  .cx.top-pinned { --r-board: var(--board-h); }
  .cx > :global(.list) { border-right: 1px solid var(--line); }
  .cx.left-unpinned > :global(.rail), .cx.left-unpinned > :global(.list) { visibility: hidden; overflow: hidden; }
  .cx.left-peek > :global(.rail) { visibility: visible; position: fixed; left: 0; top: 0; bottom: 0; width: var(--rail-w); z-index: 73; }
  .cx.left-peek > :global(.list) { visibility: visible; position: fixed; left: var(--rail-w); top: 0; bottom: 0; width: var(--list-w); z-index: 72; box-shadow: 18px 0 50px rgba(0, 0, 0, 0.14); }
  .top { grid-area: top; display: flex; align-items: flex-start; gap: 18px; padding: 12px 16px 10px 22px; min-height: var(--top-h); border-bottom: 1px solid var(--line); min-width: 0; container: top / inline-size; background: var(--bg2); }
  .main { grid-area: main; min-width: 0; min-height: 0; overflow: hidden; }
  .boardp { display: flex; flex-direction: column; min-height: 0; background: var(--bg); position: relative; }
  .boardp.pinned { grid-area: board; border-bottom: 1px solid var(--line); }
  .boardp.overlay { position: fixed; top: 0; left: calc(var(--c-rail) + var(--c-list)); right: var(--c-notes); height: 56vh; z-index: 74; border-bottom: 1px solid var(--line); box-shadow: 0 18px 50px rgba(0, 0, 0, 0.16); }
  .bph { padding: 10px 24px 0; font-size: 14px; }
  .tab { position: fixed; z-index: 60; display: flex; align-items: center; gap: 6px; background: var(--bg2); color: var(--muted); border: 1px solid var(--line); box-shadow: 0 2px 10px rgba(0, 0, 0, 0.08); cursor: pointer; font: inherit; }
  .tab:hover, .tab.on { color: var(--fg); }
  .tab.pin { z-index: 80; }
  .tab.l { left: 0; top: 50%; transform: translateY(-50%); border-left: none; border-radius: 0 10px 10px 0; padding: 12px 7px 12px 6px; }
  .tab.t { top: 0; left: calc((var(--c-rail) + var(--c-list) + 100vw - var(--c-notes)) / 2); transform: translateX(-50%); border-top: none; border-radius: 0 0 10px 10px; padding: 4px 13px 5px; }
  .tab.r { right: 0; top: 50%; transform: translateY(-50%); border-right: none; border-radius: 10px 0 0 10px; padding: 12px 6px 12px 7px; flex-direction: column; }
  .rs { position: fixed; z-index: 30; }
  .rs::after { content: ''; position: absolute; background: var(--accent); opacity: 0; transition: opacity 0.12s; }
  .rs:hover::after { opacity: 0.4; }
  .rs.v { width: 7px; cursor: col-resize; top: 0; bottom: 0; }
  .rs.v::after { top: 0; bottom: 0; left: 3px; width: 2px; }
  .rs.list { left: calc(var(--c-rail) + var(--c-list) - 4px); }
  .rs.notes { right: calc(var(--c-notes) - 4px); }
  .rs.h { position: absolute; left: 0; right: 0; bottom: -4px; height: 7px; cursor: row-resize; }
  .rs.h::after { left: 0; right: 0; top: 3px; height: 2px; }
</style>
