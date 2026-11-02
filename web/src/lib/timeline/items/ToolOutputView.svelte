<script lang="ts">
  // A tool call's output: text, a terminal transcript or a file diff.
  import type { ToolOutput } from '../../protocol';
  import Diff from '../../ui/Diff.svelte';

  let { output }: { output: ToolOutput } = $props();
</script>

{#if output.kind === 'diff'}
  <div class="path"><code>{output.path}</code></div>
  <Diff old={output.old} next={output.new} />
{:else if output.kind === 'text' || output.kind === 'terminal'}
  <pre class="out" class:term={output.kind === 'terminal'}>{output.text}</pre>
{/if}

<style>
  .out { background: var(--bg3); border-radius: 6px; padding: 6px 10px; max-height: 320px; overflow: auto; font-size: 12px; }
  .term { background: #16161a; color: #e6e6e6; }
  .path { font-size: 12px; margin-bottom: 4px; }
</style>
