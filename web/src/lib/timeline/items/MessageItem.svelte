<script lang="ts">
  // A message: the user's prompt (a bubble), the agent's reply (Markdown, patched in place while
  // it streams) or a notice message. Images and resources are shown as chips, never loaded.
  import type { ItemOf } from '../../state/timeline.svelte';
  import Markdown from '../../ui/Markdown.svelte';

  let { item }: { item: ItemOf<'message'> } = $props();
</script>

<div class="msg r-{item.role}" class:streaming={item.streaming}>
  <div class="body">
    {#each item.content as part, i (i)}
      {#if part.kind === 'text'}<Markdown text={part.text} />
      {:else if part.kind === 'image'}<span class="chip">image · {part.mime_type}</span>
      {:else if part.kind === 'resource'}<span class="chip" title={part.uri}>{part.name ?? part.uri}</span>
      {:else}<span class="chip">attachment</span>{/if}
    {/each}
    {#if item.streaming}<span class="caret" aria-label="streaming"></span>{/if}
  </div>
</div>

<style>
  .msg { display: flex; }
  .r-user { justify-content: flex-end; }
  .r-user .body { background: var(--bg3); border-radius: 14px 14px 4px 14px; padding: 8px 14px; max-width: 85%; }
  .r-agent .body { width: 100%; }
  .r-notice .body { font-size: 13px; color: var(--muted); }
  .body { min-width: 0; }
  .caret { display: inline-block; width: 7px; height: 1em; background: var(--fg); vertical-align: text-bottom; animation: blink 1s steps(2) infinite; }
  @keyframes blink { 50% { opacity: 0; } }
</style>
