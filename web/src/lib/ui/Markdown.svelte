<script lang="ts">
  // Sanitized Markdown, patched into the DOM in place: a streamed message only changes its last
  // text node, so selection and hover inside earlier paragraphs survive.
  import type { Action } from 'svelte/action';
  import { markdownFragment } from './markdown';
  import { morph } from './morph';

  let { text, inline = false }: { text: string; inline?: boolean } = $props();

  const render: Action<HTMLElement, string> = (node, source) => {
    morph(node, markdownFragment(source));
    return { update: (next: string) => morph(node, markdownFragment(next)) };
  };
</script>

<div class="md" class:inline use:render={text}></div>

<style>
  .md { overflow-wrap: anywhere; }
  .md > :global(:first-child) { margin-top: 0; }
  .md > :global(:last-child) { margin-bottom: 0; }
  .md :global(p), .md :global(ul), .md :global(ol), .md :global(blockquote), .md :global(pre), .md :global(table) { margin: 0 0 0.7em; }
  .md :global(ul), .md :global(ol) { padding-left: 1.4em; }
  .md :global(h1), .md :global(h2), .md :global(h3), .md :global(h4), .md :global(h5), .md :global(h6) { margin: 0.9em 0 0.4em; font-size: 1.05em; }
  .md :global(h1) { font-size: 1.25em; }
  .md :global(h2) { font-size: 1.15em; }
  .md :global(pre) { background: var(--bg3); border-radius: 7px; padding: 8px 10px; overflow-x: auto; white-space: pre; }
  .md :global(pre code) { background: none; padding: 0; }
  .md :global(blockquote) { border-left: 3px solid var(--line); padding-left: 10px; color: var(--muted); }
  .md :global(table) { border-collapse: collapse; font-size: 0.92em; }
  .md :global(th), .md :global(td) { border: 1px solid var(--line); padding: 3px 8px; }
  .md :global(hr) { border: none; border-top: 1px solid var(--line); }
  .inline :global(p) { display: inline; margin: 0; }
</style>
