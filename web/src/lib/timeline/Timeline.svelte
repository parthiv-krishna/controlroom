<script lang="ts">
  // The session timeline: one component per item kind, windowed (only rows near the viewport are
  // in the DOM, with measured heights in a Fenwick tree), patched in place while text streams, and
  // stuck to the bottom unless the user scrolled up. Pending requests that have no timeline item
  // (a subagent's, rolled up from the summary) follow the last row. The raw view replaces the list
  // while it is open.
  import { untrack } from 'svelte';
  import { useApp } from '../app/app.svelte';
  import { HeightIndex, visibleRange } from '../model/heights';
  import { keyOf } from '../model/projects';
  import type { MachineSession, TimelineItem } from '../protocol';
  import RequestCard from '../session/RequestCard.svelte';
  import { Timeline } from '../state/timeline.svelte';
  import { provideTimeline } from './context';
  import ItemView from './ItemView.svelte';
  import RawView from './RawView.svelte';

  let { s }: { s: MachineSession } = $props();
  const app = useApp();
  const key = $derived(keyOf(s));
  const feed = $derived(app.client.sessions.get(s.machine_id, s.id));
  const timeline = $derived(feed?.timeline ?? new Timeline());
  const rows = $derived(timeline.rows);
  const raw = $derived(app.view.raw[key] ?? null);

  provideTimeline({
    get s() {
      return s;
    },
    get timeline() {
      return timeline;
    },
    isOpen: (id, fallback) => app.view.expanded[`${key}#${id}`] ?? fallback,
    toggle(id, fallback) {
      const k = `${key}#${id}`;
      app.view.expanded[k] = !(app.view.expanded[k] ?? fallback);
    },
    showRaw: (item: TimelineItem) => (app.view.raw[key] = { from: item.seq_from, to: item.seq_to, item: item.id }),
  });

  const orphans = $derived(s.requests.filter((r) => r.state === 'pending' && !timeline.requestIds.has(r.id)));

  // ------------------------------------------------------------------------------- windowing

  /** Rendered height of a row before it is measured. */
  const ESTIMATE = 64;
  /** Extra pixels rendered above and below the viewport. */
  const OVERSCAN = 800;
  /** Within this distance of the bottom, the view follows new content. */
  const STICK_PX = 32;

  let scroller = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let viewport = $state(0);
  /** Bumped when a measured height changes. */
  let layout = $state(0);
  let stick = true;
  /** Measured heights by item id; kept for rows scrolled out of the window. */
  const measured = new Map<string, number>();

  const index = $derived(new HeightIndex(rows.map((c) => untrack(() => measured.get(c.item.id)) ?? ESTIMATE)));
  const position = $derived(new Map(rows.map((c, i) => [c.item.id, i])));
  const range = $derived.by(() => {
    void layout;
    return visibleRange(index, scrollTop, viewport, OVERSCAN);
  });
  const shown = $derived(rows.slice(range.start, range.end));
  const padTop = $derived.by(() => {
    void layout;
    return index.offset(range.start);
  });
  const padBottom = $derived.by(() => {
    void layout;
    return index.total() - index.offset(range.end);
  });

  const observer = new ResizeObserver((entries) => {
    let changed = false;
    for (const e of entries) {
      const el = e.target as HTMLElement;
      const id = el.dataset.row!;
      const h = el.offsetHeight;
      measured.set(id, h);
      const i = position.get(id);
      if (i !== undefined && i < index.length && index.set(i, h)) changed = true;
    }
    if (changed) layout++;
  });
  $effect(() => () => observer.disconnect());

  function measure(node: HTMLElement) {
    observer.observe(node);
    return { destroy: () => observer.unobserve(node) };
  }

  function onScroll() {
    if (!scroller) return;
    scrollTop = scroller.scrollTop;
    stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < STICK_PX;
  }

  $effect(() => {
    if (!scroller) return;
    const el = scroller;
    const ro = new ResizeObserver(() => (viewport = el.clientHeight));
    ro.observe(el);
    viewport = el.clientHeight;
    return () => ro.disconnect();
  });

  // Follow new content while stuck to the bottom.
  $effect(() => {
    void layout;
    void rows;
    void orphans;
    if (stick && scroller && !focusing) scroller.scrollTop = scroller.scrollHeight;
  });

  // ------------------------------------------------------------------- jumping to an item

  let focusing = false;
  $effect(() => {
    const target = app.view.focusItem;
    if (!target || target.key !== key || !scroller || raw) return;
    const row = timeline.rowOf(target.id);
    const i = row ? position.get(row.item.id) : undefined;
    if (i === undefined) return;
    untrack(() => {
      app.view.focusItem = null;
      focusing = true;
      stick = false;
      scroller!.scrollTop = Math.max(0, index.offset(i) - viewport / 3);
      requestAnimationFrame(() => {
        focusing = false;
        const el = scroller?.querySelector<HTMLElement>(`[data-item="${CSS.escape(target.id)}"]`);
        if (!el) return;
        el.scrollIntoView({ block: 'center' });
        el.classList.add('flash');
        setTimeout(() => el.classList.remove('flash'), 1500);
      });
    });
  });

  const openRaw = () => (app.view.raw[key] = { from: null, to: null, item: null });
</script>

{#if raw && feed}
  <RawView {s} {feed} focus={raw} />
{:else}
  <div class="timeline" data-pane="session" tabindex="-1" aria-label="Timeline" bind:this={scroller} onscroll={onScroll}>
    <div class="bar"><button class="btn ghost xs" title="Show the raw transcript records" onclick={openRaw} disabled={!feed}>raw</button></div>
    <div class="body">
      {#if timeline.trimmed}<div class="muted small trimmed">{timeline.trimmed} earlier items not kept</div>{/if}
      {#if !rows.length}
        <div class="placeholder">
          {#if s.preview}<p class="preview">{s.preview}</p>{/if}
          {#if s.state === 'starting'}<p class="muted"><span class="spin"></span> starting agent…</p>{/if}
        </div>
      {/if}
      <div style:height="{padTop}px" aria-hidden="true"></div>
      {#each shown as cell (cell.item.id)}
        <div class="row" data-row={cell.item.id} use:measure>
          <ItemView {cell} />
        </div>
      {/each}
      <div style:height="{padBottom}px" aria-hidden="true"></div>
      {#each orphans as req (req.id)}<div class="row"><RequestCard {req} {s} /></div>{/each}
    </div>
  </div>
{/if}

<style>
  .timeline { flex: 1; overflow-y: auto; min-height: 0; outline: none; position: relative; font-size: 15px; line-height: 1.6; }
  /* A short timeline sits at the bottom, next to the composer. */
  .body { padding: 4px 40px 16px; max-width: 900px; width: 100%; margin: 0 auto; min-height: 100%; display: flex; flex-direction: column; justify-content: flex-end; }
  .bar { position: sticky; top: 0; z-index: 2; display: flex; justify-content: flex-end; padding: 4px 8px 0; height: 0; overflow: visible; }
  .bar .btn { background: var(--bg2); font-family: var(--mono); }
  .row { padding: 7px 0; }
  .placeholder { display: flex; flex-direction: column; }
  .preview { color: var(--fg); margin: 0; }
  .trimmed { text-align: center; padding: 6px; }
  @media (max-width: 760px) { .body { padding: 4px 16px 12px; } }
</style>
