<script lang="ts">
  // The session's usage in the header, in pi's footer format (model/statusline): monospace and
  // compact. When the row it sits in is too narrow, parts are dropped (W, R, CH, …; the context
  // part stays). Tapping it, or hovering with a mouse, shows every part with its label.
  import { partsToDrop, statusParts, type PartKey } from '../model/statusline';
  import type { FeedSession } from '../protocol';
  import { dismissable } from '../ui/actions';

  let { s }: { s: FeedSession } = $props();
  const parts = $derived(statusParts(s.usage, s.context));

  let root: HTMLElement | undefined = $state();
  let line: HTMLElement | undefined = $state();
  let measure: HTMLElement | undefined = $state();
  /** The content width of the row the statusline sits in. */
  let available = $state(Infinity);
  let dropped = $state<ReadonlySet<PartKey>>(new Set());
  const shown = $derived(parts.filter((p) => !dropped.has(p.key)));

  let pinned = $state(false);
  let hovered = $state(false);
  const open = $derived(pinned || hovered);
  const close = () => (pinned = hovered = false);

  $effect(() => {
    const row = root?.parentElement;
    if (!row) return;
    const ro = new ResizeObserver(([entry]) => (available = entry.contentRect.width));
    ro.observe(row);
    return () => ro.disconnect();
  });

  // Fit: every part's width comes from a hidden copy of the full line, so what is dropped depends
  // only on the row's width and never feeds back into it.
  $effect(() => {
    if (!line || !measure || !parts.length) return;
    const widths = new Map<PartKey, number>();
    for (const el of measure.querySelectorAll<HTMLElement>('[data-part]')) widths.set(el.dataset.part as PartKey, el.getBoundingClientRect().width);
    const px = (v: string) => parseFloat(v) || 0;
    const box = getComputedStyle(line);
    const extra = px(box.paddingLeft) + px(box.paddingRight) + px(box.borderLeftWidth) + px(box.borderRightWidth);
    // A pixel of slack for sub-pixel rounding.
    dropped = partsToDrop(parts, widths, px(getComputedStyle(measure).columnGap), extra + 1, available);
  });
</script>

{#if parts.length}
  <span class="sl" bind:this={root}>
    <button
      bind:this={line}
      class="line"
      aria-label="Session usage"
      aria-haspopup="dialog"
      aria-expanded={open}
      onclick={() => (pinned = !pinned)}
      onpointerenter={(e) => e.pointerType === 'mouse' && (hovered = true)}
      onpointerleave={(e) => e.pointerType === 'mouse' && (hovered = false)}
    >
      {#each shown as p (p.key)}<span class={['p', p.level]} data-part={p.key}>{p.text}</span>{/each}
    </button>
    <span class="measure" aria-hidden="true"><span class="parts" bind:this={measure}>{#each parts as p (p.key)}<span data-part={p.key}>{p.text}</span>{/each}</span></span>
    {#if open}
      <div class="pop" role="dialog" aria-label="Session usage" use:dismissable={{ close, anchor: root }}>
        <table>
          <tbody>
            {#each parts as p (p.key)}
              <tr data-part={p.key}><td class={['p', p.level]}>{p.text}</td><th scope="row">{p.label}</th><td>{p.value}</td></tr>
            {/each}
          </tbody>
        </table>
        <div class="note">Tokens and cost add up over the whole session, across every agent run.</div>
      </div>
    {/if}
  </span>
{/if}

<style>
  .sl { position: relative; display: inline-flex; min-width: 0; max-width: 100%; }
  .line, .parts { display: inline-flex; gap: 1ch; font: 11.5px var(--mono); white-space: nowrap; }
  .line {
    min-width: 0; overflow: hidden; padding: 1px 6px; border: 1px solid transparent; border-radius: 6px;
    background: none; color: var(--muted); cursor: pointer;
  }
  .line:hover, .line[aria-expanded='true'] { color: var(--fg); border-color: var(--line); }
  .measure { position: absolute; width: 0; height: 0; overflow: hidden; visibility: hidden; }
  .p.warn { color: var(--warn); }
  .p.full { color: var(--bad); }
  .pop {
    position: absolute; z-index: 60; top: calc(100% + 6px); left: 0; width: max-content; max-width: min(420px, 86vw);
    background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 8px 12px; font-size: 12.5px;
  }
  table { border-collapse: collapse; }
  td, th { padding: 2px 10px 2px 0; text-align: left; vertical-align: baseline; }
  th { font-weight: 600; }
  td:last-child { white-space: nowrap; font-variant-numeric: tabular-nums; }
  td.p { font: 11.5px var(--mono); color: var(--muted); white-space: nowrap; }
  .note { margin-top: 6px; font-size: 11.5px; color: var(--muted); }
</style>
