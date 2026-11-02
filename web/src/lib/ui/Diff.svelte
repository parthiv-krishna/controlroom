<script lang="ts">
  // A line diff (request bodies, edit tool output).
  import { lineDiff } from '../model/misc';

  let { old, next }: { old: string; next: string } = $props();
  const lines = $derived(lineDiff(old, next));
</script>

<div class="diff" role="figure" aria-label="Diff">
  {#each lines as l, i (i)}<span class="dl {l.kind}">{l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '} {l.text}</span>{/each}
</div>

<style>
  .diff { background: var(--bg3); border-radius: 6px; padding: 6px 0; overflow: auto; max-height: 320px; font: 12.5px var(--mono); }
  .dl { display: block; padding: 0 10px; white-space: pre; }
  .dl.add { background: color-mix(in srgb, var(--ok) 15%, transparent); color: var(--dadd); }
  .dl.del { background: color-mix(in srgb, var(--bad) 15%, transparent); color: var(--ddel); }
  .dl.ctx { color: var(--muted); }
</style>
