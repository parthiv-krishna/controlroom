<script lang="ts">
  // The session pane: banners, the jobs strip, the timeline slot, the todo panel, schedules, the
  // queue and the composer. The header is in the top bar on desktop and inline on the phone.
  import { useApp } from '../app/app.svelte';
  import { keyOf } from '../model/projects';
  import type { MachineSession } from '../protocol';
  import Banners from './Banners.svelte';
  import Composer from './Composer.svelte';
  import JobsStrip from './JobsStrip.svelte';
  import QueuePanel from './QueuePanel.svelte';
  import SchedulesPanel from './SchedulesPanel.svelte';
  import SessionHeader from './SessionHeader.svelte';
  import TimelineSlot from './TimelineSlot.svelte';
  import TodoPanel from './TodoPanel.svelte';

  let { s, header = false }: { s: MachineSession; header?: boolean } = $props();
  const app = useApp();
  const detail = $derived(app.client.sessions.get(s.machine_id, s.id)?.detail ?? null);
  const key = $derived(keyOf(s));
  const hasSchedules = $derived(app.client.feed.scheduleList.some((x) => x.machine_id === s.machine_id && x.session_id === s.id));
  const showSchedules = $derived(app.view.panels['schedules:' + key] ?? hasSchedules);
</script>

<div class="pane">
  {#if header}<div class="ph"><SessionHeader {s} {detail} /></div>{/if}
  <Banners {s} m={app.client.feed.machines.get(s.machine_id)} />
  {#if detail?.jobs.length}<JobsStrip {s} jobs={detail.jobs} />{/if}
  <TimelineSlot {s} />
  {#if detail && (detail.todos.length || detail.auto_continue.status !== 'off')}<TodoPanel {s} {detail} />{/if}
  {#if showSchedules}<SchedulesPanel {s} />{/if}
  {#if detail?.queue.length || s.queue.paused}<QueuePanel {s} items={detail?.queue ?? []} />{/if}
  <Composer {s} />
</div>

<style>
  .pane { display: flex; flex-direction: column; height: 100%; min-height: 0; background: var(--bg2); }
  .ph { padding: 6px 16px; }
</style>
