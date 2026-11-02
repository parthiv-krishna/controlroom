<script lang="ts">
  // The raw view: the session's transcript records (opt-in raw frames, streamed while this view is
  // open). Opened from an item's `#seq` link it loads the records around the item and highlights
  // its range; each record links back to the innermost timeline item it was folded into.
  import { onDestroy } from 'svelte';
  import { useApp } from '../app/app.svelte';
  import type { RawFocus } from '../app/view.svelte';
  import { keyOf } from '../model/projects';
  import type { MachineSession, RawFrame, RawStream } from '../protocol';
  import type { SessionFeed } from '../state/session-feeds.svelte';

  let { s, feed, focus }: { s: MachineSession; feed: SessionFeed; focus: RawFocus } = $props();
  const app = useApp();
  const key = $derived(keyOf(s));
  const raw = $derived(feed.timeline.raw);

  $effect(() => app.client.sessions.showRaw(feed, focus.from));
  onDestroy(() => app.client.sessions.hideRaw(feed));

  type Filter = 'all' | RawStream;
  const FILTERS: [Filter, string][] = [['all', 'All'], ['to_agent', '→ agent'], ['from_agent', '← agent'], ['stderr', 'stderr'], ['meta', 'meta']];
  const STREAM: Record<RawStream, string> = { to_agent: '→ agent', from_agent: '← agent', stderr: 'stderr', meta: 'meta', unknown: '?' };
  let filter = $state<Filter>('all');
  let open = $state<Record<number, boolean>>({});
  const shown = $derived(filter === 'all' ? raw.frames : raw.frames.filter((f) => f.stream === filter));
  const inFocus = (seq: number) => focus.from !== null && focus.to !== null && seq >= focus.from && seq <= focus.to;
  const heading = $derived(
    [`${raw.frames.length} records`, focus.from === null ? null : `item #${focus.from}${focus.to !== focus.from ? `–${focus.to}` : ''}`].filter(Boolean).join(' · '),
  );

  /** A one-line summary: the JSON `type` (and method or command) when the line is an object. */
  function summary(f: RawFrame): string {
    try {
      const v: unknown = JSON.parse(f.text);
      if (v && typeof v === 'object' && !Array.isArray(v)) {
        const o = v as Record<string, unknown>;
        const parts = ['type', 'method', 'command', 'kind'].map((k) => o[k]).filter((x): x is string => typeof x === 'string');
        if (parts.length) return parts.join(' · ');
      }
    } catch {
      // Not JSON: show the text itself.
    }
    return f.text.length > 160 ? f.text.slice(0, 160) + '…' : f.text;
  }
  function body(f: RawFrame): string {
    try {
      return JSON.stringify(JSON.parse(f.text), null, 2);
    } catch {
      return f.text;
    }
  }
  const time = (ts: number) => new Date(ts).toLocaleTimeString([], { hour12: false });

  /** The innermost item whose records include `seq` (the latest-starting one that contains it). */
  function ownerOf(seq: number): string | null {
    let best: string | null = null;
    for (const cell of feed.timeline.all()) {
      const it = cell.item;
      if (it.seq_from > seq) break;
      if (it.seq_to >= seq) best = it.id;
    }
    return best;
  }
  function toItem(seq: number) {
    const id = ownerOf(seq);
    if (!id) return app.toasts.show('No timeline item for this record', 'error');
    app.view.focusItem = { key, id };
    delete app.view.raw[key];
  }
  const back = () => {
    if (focus.item) app.view.focusItem = { key, id: focus.item };
    delete app.view.raw[key];
  };

  // Scroll to the focused records once they are loaded.
  let list = $state<HTMLDivElement>();
  let scrolledTo: number | null = null;
  $effect(() => {
    const from = focus.from;
    if (from === null || from === scrolledTo || !list || !raw.frames.some((f) => f.seq >= from)) return;
    const row = list.querySelector<HTMLElement>(`[data-seq="${from}"]`) ?? list.querySelector<HTMLElement>('.focus');
    if (!row) return;
    scrolledTo = from;
    row.scrollIntoView({ block: 'center' });
  });
</script>

<div class="raw" aria-label="Raw transcript" data-pane="session" tabindex="-1">
  <div class="bar">
    <button class="btn sm" onclick={back}><span class="kbd">←</span> Timeline</button>
    <span class="muted small">{heading}</span>
    <span class="sp"></span>
    <div class="seg" role="group" aria-label="Filter records">
      {#each FILTERS as [v, l] (v)}<button class:on={filter === v} onclick={() => (filter = v)}>{l}</button>{/each}
    </div>
  </div>
  <div class="list" bind:this={list}>
    {#if raw.clipped}<div class="muted small note">{raw.anchored ? 'Later records are not loaded; open the raw view from a later item to see them.' : 'Older records are not kept.'}</div>{/if}
    {#each shown as f (f.seq)}
      <div class="line s-{f.stream}" class:focus={inFocus(f.seq)} data-seq={f.seq}>
        <button class="rec" aria-expanded={!!open[f.seq]} onclick={() => (open[f.seq] = !open[f.seq])}>
          <span class="seq">#{f.seq}</span><span class="t">{time(f.ts)}</span><span class="dir">{STREAM[f.stream]}</span><span class="sum">{summary(f)}</span>
        </button>
        <button class="to-item" title="Show the timeline item this record belongs to" onclick={() => toItem(f.seq)}>item ↗</button>
      </div>
      {#if open[f.seq]}<pre class="body">{body(f)}</pre>{/if}
    {:else}
      <div class="muted small note">Loading records…</div>
    {/each}
  </div>
</div>

<style>
  .raw { flex: 1; display: flex; flex-direction: column; min-height: 0; outline: none; }
  .bar { display: flex; align-items: center; gap: 10px; padding: 8px 16px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: 7px; overflow: hidden; }
  .seg button { background: none; border: none; padding: 3px 9px; font: 12px var(--font); color: var(--muted); cursor: pointer; }
  .seg button.on { background: var(--bg3); color: var(--fg); }
  .list { flex: 1; overflow: auto; padding: 6px 0 16px; font: 12px var(--mono); }
  .line { display: flex; align-items: center; padding: 0 16px; }
  .line:hover { background: var(--hover); }
  .line.focus { background: color-mix(in srgb, var(--warn) 14%, transparent); }
  .rec { flex: 1; min-width: 0; display: flex; gap: 10px; background: none; border: none; font: inherit; color: inherit; padding: 2px 0; text-align: left; cursor: pointer; }
  .seq { color: var(--muted); width: 52px; flex: none; }
  .t { color: var(--muted); flex: none; }
  .dir { width: 64px; flex: none; }
  .s-to_agent .dir { color: var(--info); }
  .s-stderr .dir { color: var(--bad); }
  .s-meta .dir { color: var(--muted); }
  .sum { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .to-item { background: none; border: none; color: var(--muted); font: inherit; cursor: pointer; flex: none; opacity: 0.6; }
  .line:hover .to-item, .to-item:focus-visible { opacity: 1; color: var(--fg); }
  .body { margin: 2px 16px 6px 78px; background: var(--bg3); border-radius: 6px; padding: 6px 10px; max-height: 360px; overflow: auto; }
  .note { padding: 6px 16px; font-family: var(--font); }
</style>
