<script lang="ts">
  // A change of the shared todo list, as one line ("Todo: 2 done, 1 added"); the list itself is in
  // the todo panel, which a click opens.
  import { useApp } from '../../app/app.svelte';
  import { todoDelta } from '../../model/todos';
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'todo'> } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const delta = $derived(todoDelta(ctx.timeline.todosBefore.get(item.id) ?? [], item.items));
</script>

<button class="todo" title="Show the todo panel" onclick={() => (app.view.panels['todo:' + ctx.s.id] = true)}>
  <span aria-hidden="true">☑</span> Todo: {delta}
</button>

<style>
  .todo { display: inline-flex; align-items: center; gap: 6px; background: none; border: none; padding: 0; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; text-align: left; }
  .todo:hover { color: var(--fg); }
</style>
