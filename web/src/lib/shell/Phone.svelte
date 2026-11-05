<script lang="ts">
  // Phone (≤ 760px): a header with the project switcher and the bell, bottom tabs (Sessions ·
  // Tasks), the list then a full-screen conversation with ‹ back; menus and dialogs are sheets.
  import { useApp } from '../app/app.svelte';
  import SessionList from '../list/SessionList.svelte';
  import { notes } from '../model/notifications';
  import { UNASSIGNED, needsYouElsewhere, projectCode, scopeName } from '../model/projects';
  import NotificationsPanel from '../notifications/NotificationsPanel.svelte';
  import SessionPane from '../session/SessionPane.svelte';
  import TasksPane from '../tasks/TasksPane.svelte';
  import Icon from '../ui/Icon.svelte';
  import Drawer from './Drawer.svelte';

  const app = useApp();
  const view = app.view;
  const cur = $derived(app.current);
  const p = $derived(app.project);
  const n = $derived(notes(app.ctx, app.scope));
  const hot = $derived(n.blocking.length + n.problems.length > 0);
  const elsewhere = $derived(needsYouElsewhere(app.ctx, app.scope));
  const detail = $derived(view.page === 'sessions' && view.phoneDetail && !!cur);
</script>

<div class="phone" class:detail>
  {#if !detail}
    <header class="top">
      <button class="proj" style:--pc={p?.color ?? 'var(--fg)'} aria-label="Projects" onclick={() => (view.drawer = true)}>
        <span class="ic" class:home={!p}>{p ? projectCode(p) : app.scope === UNASSIGNED ? '○' : '∗'}</span><span class="nm">{scopeName(app.scope)}</span><span class="muted">▾</span>
        {#if elsewhere}<span class="else" title="needs you in other projects">{elsewhere}</span>{/if}
      </button>
      <button class="bell" class:on={view.notesOpen} aria-label="Notifications" onclick={() => app.toggleNotes()}>
        <Icon name="bell" size={18} />{#if n.total}<span class="bn" class:hot>{n.total}</span>{/if}
      </button>
    </header>
  {/if}
  {#if view.drawer}<Drawer />{/if}
  {#if view.notesOpen}<NotificationsPanel overlay />{/if}
  <main class="main">
    {#if view.page === 'tasks'}
      <TasksPane />
    {:else if detail && cur}
      <div class="back"><button onclick={() => (view.phoneDetail = false)}>‹ Sessions</button></div>
      <div class="conv"><SessionPane s={cur} header /></div>
    {:else}
      <SessionList title={false} />
    {/if}
  </main>
  {#if !detail}
    <nav class="tabs" aria-label="Pages">
      <button class:on={view.page === 'sessions'} onclick={() => { view.page = 'sessions'; view.phoneDetail = false; }}>Sessions</button>
      <button class:on={view.page === 'tasks'} onclick={() => (view.page = 'tasks')}>Tasks</button>
    </nav>
  {/if}
</div>

<style>
  .phone { display: grid; height: 100%; grid-template-rows: auto minmax(0, 1fr) auto; }
  .phone.detail { grid-template-rows: minmax(0, 1fr); }
  .top { display: flex; align-items: center; gap: 8px; padding: env(safe-area-inset-top) 12px 0; height: calc(52px + env(safe-area-inset-top)); border-bottom: 1px solid var(--line); }
  .proj { display: flex; align-items: center; gap: 10px; background: none; border: none; color: var(--fg); font: inherit; font-size: 16px; font-weight: 600; padding: 6px 4px; cursor: pointer; min-width: 0; flex: 1; }
  .ic { width: 30px; height: 30px; border-radius: 9px; display: grid; place-items: center; font-size: 12px; font-weight: 700; flex: none; background: var(--pc); color: #fff; text-transform: capitalize; }
  .ic.home { background: var(--bg3); color: var(--muted); }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .else { background: var(--warn); color: #fff; border-radius: 999px; font-size: 11px; font-weight: 700; padding: 1px 7px; }
  .bell { position: relative; background: none; border: none; color: var(--muted); width: 40px; height: 40px; border-radius: 9px; display: grid; place-items: center; cursor: pointer; }
  .bell.on { background: var(--bg3); color: var(--fg); }
  .bn { position: absolute; top: 2px; right: 1px; min-width: 16px; height: 16px; padding: 0 4px; border-radius: 8px; background: var(--bg3); color: var(--fg); font-size: 10px; font-weight: 700; display: grid; place-items: center; }
  .bn.hot { background: var(--warn); color: #fff; }
  .main { min-height: 0; overflow: hidden; display: flex; flex-direction: column; }
  .main > :global(.list) { flex: 1; }
  .back { display: flex; align-items: center; gap: 10px; padding: calc(env(safe-area-inset-top) + 8px) 18px 0 8px; }
  .back button { background: none; border: none; color: var(--fg); font: inherit; font-size: 16px; padding: 8px 10px; cursor: pointer; }
  .conv { flex: 1; min-height: 0; }
  .tabs { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; padding: 6px 10px calc(env(safe-area-inset-bottom) + 6px); border-top: 1px solid var(--line); background: var(--bg2); }
  .tabs button { height: 44px; border-radius: 12px; border: none; background: none; color: var(--muted); font: inherit; font-size: 14px; cursor: pointer; }
  .tabs button.on { background: var(--bg3); color: var(--fg); font-weight: 600; }
</style>
