<script lang="ts">
  // The session's schedules (spec §8.1): the server wakes the agent with a canned message on a cron
  // or interval spec. Add, edit, enable, run now, delete; next run and recent runs come from the
  // server.
  import { useApp } from '../app/app.svelte';
  import { formatIn, humanSchedule, localZone, parseCron } from '../model/schedule';
  import { ago } from '../model/status';
  import type { IfBusy, MachineSession, MissedPolicy, Schedule, ScheduleSpec } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';

  let { s }: { s: MachineSession } = $props();
  const app = useApp();
  const list = $derived(app.client.feed.scheduleList.filter((x) => x.machine_id === s.machine_id && x.session_id === s.id));
  const online = $derived(app.client.online);
  let history = $state<string | null>(null);

  interface Draft {
    id: string | null;
    name: string;
    kind: 'cron' | 'interval';
    expr: string;
    every: number;
    unit: 'min' | 'h';
    timezone: string;
    message: string;
    if_busy: IfBusy;
    missed: MissedPolicy;
  }
  let draft = $state<Draft | null>(null);

  function edit(x: Schedule | null) {
    const sp = x?.spec;
    const hours = sp?.kind === 'interval' && sp.every_s % 3600 === 0;
    draft = {
      id: x?.id ?? null,
      name: x?.name ?? '',
      kind: sp?.kind === 'interval' ? 'interval' : 'cron',
      expr: sp?.kind === 'cron' ? sp.expr : '0 9 * * 1-5',
      every: sp?.kind === 'interval' ? sp.every_s / (hours ? 3600 : 60) : 2,
      unit: sp?.kind === 'interval' && !hours ? 'min' : 'h',
      timezone: x?.timezone ?? localZone(),
      message: x?.message ?? '',
      if_busy: x?.if_busy ?? 'queue',
      missed: x?.missed ?? 'skip',
    };
  }
  const spec = $derived<ScheduleSpec | null>(
    draft ? (draft.kind === 'cron' ? { kind: 'cron', expr: draft.expr.trim() } : { kind: 'interval', every_s: Math.max(1, draft.every) * (draft.unit === 'h' ? 3600 : 60) }) : null,
  );
  const cronOk = $derived(spec?.kind !== 'cron' || !!parseCron(spec.expr));
  const valid = $derived(!!draft && !!spec && !!draft.name.trim() && !!draft.message.trim() && cronOk);

  async function save() {
    if (!draft || !spec || !valid) return;
    const fields = { name: draft.name.trim(), spec, timezone: draft.timezone.trim() || 'UTC', message: draft.message.trim(), if_busy: draft.if_busy, missed: draft.missed };
    const r = draft.id
      ? await app.client.call('schedules.update', { schedule_id: draft.id, ...fields })
      : await app.client.call('schedules.create', { machine_id: s.machine_id, session_id: s.id, ...fields });
    if (r) draft = null;
  }
  function editorKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      draft = null;
    } else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void save();
    }
  }
  async function remove(x: Schedule) {
    if (await app.dialogs.confirm({ title: `Delete schedule “${x.name}”?`, body: 'Past runs stay in the timeline.', ok: 'Delete', danger: true })) await app.client.call('schedules.delete', { schedule_id: x.id });
  }
  const OUTCOME: Record<string, string> = { fired: 'fired', queued: 'queued (busy)', skipped: 'skipped', missed: 'missed', failed: 'failed', unknown: '?' };
</script>

<div class="sched" aria-label="Schedules" data-pane="schedules">
  <div class="sh">
    <b>Schedules</b> <span class="muted">{list.length}</span><span class="sp"></span>
    {#if !draft}<button class="btn xs" disabled={!online} onclick={() => edit(null)}>+ Add schedule</button>{/if}
  </div>
  {#each list as x (x.id)}
    {@const last = x.recent_runs[0]}
    <div class="row" class:off={!x.enabled} data-schedule={x.id}>
      <input type="checkbox" checked={x.enabled} aria-label="Enable {x.name}" disabled={!online} onchange={(e) => app.client.call('schedules.update', { schedule_id: x.id, enabled: e.currentTarget.checked })} />
      <div class="main">
        <div><b>{x.name}</b> <span class="when">{humanSchedule(x.spec)} · {x.timezone}</span></div>
        <div class="small muted msg">“{x.message}”</div>
        <div class="small">
          {#if x.enabled && x.next_run}next {formatIn(x.next_run, x.timezone)}{:else}<span class="muted">disabled</span>{/if}
          {#if last}<span class="muted"> · last</span> <button class="out o-{last.outcome}" onclick={() => (history = history === x.id ? null : x.id)}>{OUTCOME[last.outcome]}{last.count > 1 ? ` ×${last.count}` : ''}</button> <span class="muted">{ago(last.at)}</span>{/if}
          <span class="muted"> · if busy: {x.if_busy} · after downtime: {x.missed === 'run_once' ? 'run once' : 'skip'}</span>
        </div>
        {#if history === x.id}
          <div class="hist small">
            {#each x.recent_runs as r (r.at)}<div><span class="out o-{r.outcome}">{OUTCOME[r.outcome]}{r.count > 1 ? ` ×${r.count}` : ''}</span> <span class="muted">{formatIn(r.at, x.timezone)}{r.detail ? ` · ${r.detail}` : ''}</span></div>{/each}
          </div>
        {/if}
      </div>
      <div class="acts">
        <button class="btn xs" disabled={!online} title="Send the message now (same busy rule)" onclick={() => app.client.call('schedules.run_now', { schedule_id: x.id })}>Run now</button>
        <button class="btn xs ghost" disabled={!online} onclick={() => edit(x)}>Edit</button>
        <button class="btn xs ghost bad-t" disabled={!online} onclick={() => remove(x)}>Delete</button>
      </div>
    </div>
  {:else}
    {#if !draft}<div class="small muted">No schedules.</div>{/if}
  {/each}
  {#if draft}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions (Esc and Ctrl-Enter from its fields) -->
    <div class="editor" role="group" aria-label="Schedule editor" onkeydown={editorKey}>
      <div class="er">
        <input class="name" placeholder="Name" aria-label="Schedule name" bind:value={draft.name} />
        <Dropdown small label="Kind" value={draft.kind} options={[{ value: 'cron', label: 'Cron' }, { value: 'interval', label: 'Every' }]} onchange={(v) => draft && (draft.kind = v as Draft['kind'])} />
        {#if draft.kind === 'cron'}
          <input class="mono expr" aria-label="Cron expression" placeholder="0 9 * * 1-5" bind:value={draft.expr} />
        {:else}
          <input class="num" type="number" min="1" aria-label="Interval" bind:value={draft.every} />
          <Dropdown small label="Unit" value={draft.unit} options={[{ value: 'min', label: 'minutes' }, { value: 'h', label: 'hours' }]} onchange={(v) => draft && (draft.unit = v as Draft['unit'])} />
        {/if}
        <input class="tz" aria-label="Time zone" bind:value={draft.timezone} />
      </div>
      <div class="small" class:bad-t={!cronOk}>{spec ? (cronOk ? humanSchedule(spec) : 'Invalid cron expression (minute hour day month weekday)') : ''}</div>
      <textarea rows="2" placeholder="Message sent to the agent" aria-label="Schedule message" bind:value={draft.message}></textarea>
      <div class="er">
        <span class="small muted">If busy</span>
        <Dropdown small label="If busy" value={draft.if_busy} options={[{ value: 'queue', label: 'Queue the message' }, { value: 'skip', label: 'Skip this run' }]} onchange={(v) => draft && (draft.if_busy = v as IfBusy)} />
        <span class="small muted">After downtime</span>
        <Dropdown small label="Missed runs" value={draft.missed} options={[{ value: 'skip', label: 'Skip missed runs' }, { value: 'run_once', label: 'Run once' }]} onchange={(v) => draft && (draft.missed = v as MissedPolicy)} />
        <span class="sp"></span>
        <button class="btn sm" onclick={() => (draft = null)}><span class="kbd">Esc</span> Cancel</button>
        <button class="btn sm primary" disabled={!valid || !online} onclick={save}>Save</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .sched { margin: 0 auto; max-width: 820px; width: calc(100% - 80px); border-top: 1px solid var(--line); padding: 6px 0 4px; font-size: 13px; }
  .sh { display: flex; align-items: center; gap: 8px; }
  .row { display: flex; gap: 10px; align-items: flex-start; padding: 8px 0; border-bottom: 1px dashed var(--line); }
  .row.off { opacity: 0.6; }
  .row input { margin-top: 3px; }
  .main { flex: 1; min-width: 0; }
  .when { font-size: 12px; color: var(--muted); }
  .msg { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .out { background: none; border: none; padding: 0; font: inherit; cursor: pointer; text-decoration: underline dotted; color: inherit; }
  .o-missed, .o-failed { color: var(--bad); }
  .o-fired { color: var(--ok); }
  .hist { margin-top: 4px; padding-left: 8px; border-left: 2px solid var(--line); }
  .acts { display: flex; gap: 4px; flex-wrap: wrap; }
  .editor { display: flex; flex-direction: column; gap: 6px; padding: 8px 0; }
  .er { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .er input { width: auto; padding: 3px 8px; }
  .name { flex: 1; min-width: 140px; }
  .expr { width: 130px; font-size: 12.5px; }
  .num { width: 70px; }
  .tz { width: 150px; }
  @media (max-width: 760px) { .sched { width: auto; margin: 0 12px; } }
</style>
