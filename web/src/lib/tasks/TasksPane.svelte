<script lang="ts">
  // The tasks board with its filter and, when a task is open, the detail pane beside it.
  import { useApp } from '../app/app.svelte';
  import Board from './Board.svelte';
  import TaskDetail from './TaskDetail.svelte';

  const app = useApp();
  const open = $derived(app.view.openTask ? app.client.feed.tasks.get(app.view.openTask) : undefined);
</script>

<div class="tp" class:detail={!!open}>
  <div class="bw">
    <div class="bh">
      <input class="q" placeholder="Filter tasks" aria-label="Filter tasks" bind:value={app.view.boardQ} />
      <span class="sp"></span>
      <button class="btn sm" onclick={() => app.newTask('todo')}>+ Task</button>
    </div>
    <Board />
  </div>
  {#if open}<aside class="side"><TaskDetail t={open} onclose={() => (app.view.openTask = null)} /></aside>{/if}
</div>

<style>
  .tp { display: grid; grid-template-columns: minmax(0, 1fr); height: 100%; min-height: 0; }
  .tp.detail { grid-template-columns: minmax(0, 1fr) 440px; }
  .bw { display: flex; flex-direction: column; padding: 10px 24px 12px; min-height: 0; min-width: 0; gap: 10px; }
  .bh { display: flex; align-items: center; gap: 8px; }
  .q { max-width: 300px; border: none; background: var(--bg3); border-radius: 8px; padding: 6px 10px; }
  .side { border-left: 1px solid var(--line); min-height: 0; }
  @media (max-width: 760px) {
    .tp.detail { grid-template-columns: 1fr; }
    .bw { padding: 10px 0 0; }
    .bh { padding: 0 12px; }
    .q { max-width: none; font-size: 16px; }
    .side { position: fixed; inset: 0; z-index: 60; padding-top: env(safe-area-inset-top); border: none; }
  }
</style>
