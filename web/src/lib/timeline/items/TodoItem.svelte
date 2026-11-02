<script lang="ts">
  // The shared todo list after a change.
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'todo'> } = $props();
  const MARK = { pending: '☐', in_progress: '◐', done: '☑', blocked: '⊘', skipped: '–', unknown: '?' } as const;
</script>

<div class="todo">
  <div class="muted small">Todo list</div>
  {#each item.items as t (t.id)}
    <div class="t s-{t.status}"><span aria-hidden="true">{MARK[t.status]}</span> {t.text}{#if t.reason}<span class="muted"> · {t.reason}</span>{/if}</div>
  {/each}
</div>

<style>
  .todo { font-size: 13.5px; border-left: 2px solid var(--line); padding-left: 10px; }
  .s-done, .s-skipped { color: var(--muted); }
  .s-done { text-decoration: line-through; }
  .s-blocked { color: var(--warn); }
</style>
