<script lang="ts">
  // Five columns with drag and drop: dropping a card before another card (or at the column's end)
  // sends tasks.move with a fractional order, so only the moved task changes.
  import { useApp } from '../app/app.svelte';
  import { taskIn } from '../model/projects';
  import { TASK_COLUMNS, columnTasks, orderFor } from '../model/tasks';
  import type { Task, TaskStatus } from '../protocol';
  import TaskCard from './TaskCard.svelte';

  const app = useApp();
  let overCol = $state<TaskStatus | null>(null);

  const tasks = $derived.by(() => {
    const q = app.view.boardQ.trim().toLowerCase();
    return app.ctx.tasks.filter((t) => taskIn(t, app.scope) && (!q || t.title.toLowerCase().includes(q)));
  });

  function move(t: Task, status: TaskStatus, before: string | null = null) {
    const order = orderFor(columnTasks(app.ctx.tasks, status), t.id, before);
    void app.client.call('tasks.move', { task_id: t.id, status, order });
  }
  function drop(e: DragEvent, status: TaskStatus) {
    overCol = null;
    const id = e.dataTransfer?.getData('application/x-cr-task');
    const t = id ? app.client.feed.tasks.get(id) : undefined;
    if (!t) return;
    e.preventDefault();
    const before = (e.target as HTMLElement).closest<HTMLElement>('[data-task]')?.dataset.task ?? null;
    if (before !== t.id) move(t, status, before);
  }
</script>

<div class="board" data-pane="board">
  {#each TASK_COLUMNS as c (c.id)}
    {@const list = columnTasks(tasks, c.id)}
    <section
      class="col"
      class:over={overCol === c.id}
      aria-label={c.label}
      data-column={c.id}
      ondragover={(e) => { if (e.dataTransfer?.types.includes('application/x-cr-task')) { e.preventDefault(); overCol = c.id; } }}
      ondragleave={(e) => { if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node)) overCol = null; }}
      ondrop={(e) => drop(e, c.id)}
    >
      <div class="ch"><span class="cl">{c.icon} {c.label}</span><span class="muted">{list.length}</span><button class="btn xs ghost" aria-label="Add task to {c.label}" onclick={() => app.newTask(c.id)}>+</button></div>
      <div class="cb">
        {#each list as t (t.id)}<TaskCard {t} onmove={(status) => move(t, status)} />{:else}<div class="drop">drop here</div>{/each}
      </div>
    </section>
  {/each}
</div>

<style>
  .board { display: grid; grid-auto-flow: column; grid-auto-columns: minmax(220px, 1fr); gap: 10px; min-height: 0; height: 100%; overflow-x: auto; }
  .col { display: flex; flex-direction: column; min-height: 0; border-radius: 10px; border: 1px solid transparent; }
  .col.over { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 6%, var(--bg2)); }
  .ch { display: flex; align-items: center; gap: 6px; padding: 8px 10px 4px; font-size: 13px; color: var(--muted); }
  .cl { flex: 1; font-weight: 500; }
  .cb { flex: 1; overflow-y: auto; padding: 4px 6px 10px; display: flex; flex-direction: column; gap: 7px; min-height: 40px; }
  .drop { color: var(--muted); font-size: 12px; text-align: center; padding: 14px; border: 1px dashed var(--line); border-radius: 8px; }
  @media (max-width: 760px) { .board { grid-auto-columns: 84vw; scroll-snap-type: x mandatory; padding: 0 12px; } .col { scroll-snap-align: start; } }
</style>
