<script lang="ts">
  // The session pane: banners, the jobs strip, the timeline slot, the todo panel, schedules, the
  // queue and the composer. The header is in the top bar on desktop and inline on the phone.
  import { useApp } from '../app/app.svelte';
  import type { FeedSession } from '../protocol';
  import Timeline from '../timeline/Timeline.svelte';
  import Banners from './Banners.svelte';
  import Composer from './Composer.svelte';
  import JobsStrip from './JobsStrip.svelte';
  import QueuePanel from './QueuePanel.svelte';
  import SchedulesPanel from './SchedulesPanel.svelte';
  import SessionHeader from './SessionHeader.svelte';
  import TodoPanel from './TodoPanel.svelte';

  let { s, header = false }: { s: FeedSession; header?: boolean } = $props();
  const app = useApp();
  const detail = $derived(app.detail(s));
  const key = $derived(s.id);
  const hasSchedules = $derived(app.client.feed.scheduleList.some((x) => x.session_id === s.id));
  // Auto-continue is on by default: the panel shows once there are todos, or while it is paused.
  const PAUSED = new Set(['paused', 'stalled', 'budget']);
  const showSchedules = $derived(app.view.panels['schedules:' + key] ?? hasSchedules);
</script>

<div class="pane">
  {#if header}<div class="ph"><SessionHeader {s} {detail} /></div>{/if}
  <Banners {s} />
  {#if detail?.jobs.length}<JobsStrip {s} jobs={detail.jobs} />{/if}
  {#key key}<Timeline {s} />{/key}
  {#if detail && (detail.todos.length || PAUSED.has(detail.auto_continue.status))}<TodoPanel {s} {detail} />{/if}
  {#if showSchedules}<SchedulesPanel {s} />{/if}
  {#if detail?.queue.length || s.queue.paused}<QueuePanel {s} items={detail?.queue ?? []} />{/if}
  <Composer {s} />
</div>

<style>
  .pane { display: flex; flex-direction: column; height: 100%; min-height: 0; background: var(--bg2); }
  .ph { padding: 6px 16px; }
</style>
