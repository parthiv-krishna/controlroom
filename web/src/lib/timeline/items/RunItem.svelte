<script lang="ts">
  // A run marker. A run you started (a message, or one sent from the queue) that ended normally is
  // just a gap between turns; its trigger and time are in its title. Any other trigger (steer,
  // injection, auto-continue, retry, extension, schedule) or outcome shows a header line. The
  // run's provider retries are folded in as one quiet line ("retried 2×", errors in its title);
  // the error shows when the last attempt gave up.
  import { useApp } from '../../app/app.svelte';
  import type { DeviceRef, RunTrigger } from '../../protocol';
  import type { ItemOf } from '../../state/timeline.svelte';
  import { useTimeline } from '../context';

  let { item }: { item: ItemOf<'run'> } = $props();
  const app = useApp();
  const ctx = useTimeline();
  const OUTCOME = { completed: 'done', cancelled: 'stopped', failed: 'failed', max_tokens: 'max tokens', refusal: 'refused', unknown: 'ended' } as const;
  const INJECT = { user: 'user', job: 'job', child: 'subagent', message: 'message', answer: 'answer', todo: 'todo edit', continue: 'continue', tool_late: 'late tool result', workspace: 'workspace notice', unknown: 'injected' } as const;

  /** The device's current label, falling back to the label at the time. */
  const device = (d: DeviceRef | null) => (d ? (app.client.feed.devices.get(d.device_id)?.label ?? d.label) : null);

  function trigger(t: RunTrigger): { label: string; title: string } {
    switch (t.kind) {
      case 'user': {
        const d = device(t.device);
        return { label: d ?? 'message', title: d ? `Started by a message from ${d}` : 'Started by a message' };
      }
      case 'queue': {
        const d = device(t.device);
        return { label: 'queue', title: d ? `Sent from the queue (queued on ${d})` : 'Sent from the queue' };
      }
      case 'steer': {
        const d = device(t.device);
        return { label: d ? `steer · ${d}` : 'steer', title: 'Steered' };
      }
      case 'inject':
        return { label: t.label || INJECT[t.origin], title: `Woken by an injected ${INJECT[t.origin]}` };
      case 'auto_continue':
        return { label: 'auto-continue', title: 'Auto-continue: open todos remain' };
      case 'retry':
        return { label: 'retry', title: 'Automatic retry' };
      case 'extension':
        return { label: `extension: ${t.name}`, title: `Started by the extension "${t.name}"` };
      case 'schedule':
        return { label: `schedule: ${t.name}`, title: `Started by the schedule "${t.name}"` };
      default:
        return { label: 'run', title: 'Started by the agent' };
    }
  }
  const t = $derived(trigger(item.trigger));
  const when = $derived(new Date(item.ts).toLocaleString());
  /** Started by you, and running or ended normally. */
  const quiet = $derived((item.trigger.kind === 'user' || item.trigger.kind === 'queue') && (item.outcome === null || item.outcome === 'completed'));

  const retries = $derived((ctx.timeline.retries.get(item.id) ?? []).map((c) => c.item).filter((r) => r.kind === 'retry'));
  const attempts = $derived(retries.reduce((n, r) => Math.max(n, r.attempt), 0));
  const retrying = $derived(retries.some((r) => r.phase === 'waiting' || r.phase === 'retrying'));
  const gaveUp = $derived(retries.findLast((r) => r.phase === 'gave_up'));
  const errors = $derived([...new Set(retries.map((r) => r.error).filter((e) => e !== null))].join('\n'));
</script>

{#snippet retried()}
  {#if retries.length}
    <span class="retried" class:bad={!!gaveUp} title={errors || undefined}>
      {#if retrying}<span class="spin"></span> retrying{:else}↻ retried {attempts}×{/if}
    </span>
  {/if}
{/snippet}

{#if quiet}
  <div class="run quiet" class:bare={!retries.length} title="{t.title} · {when}">{@render retried()}</div>
{:else}
  <div class="run o-{item.outcome ?? 'none'}" class:running={item.state === 'running'} title={when}>
    <span class="trig t-{item.trigger.kind}" title={t.title}>{t.label}</span>
    <span class="st">
      {#if item.state === 'running'}<span class="spin"></span> running{:else}{OUTCOME[item.outcome ?? 'unknown']}{/if}
    </span>
    {@render retried()}
    {#if item.error}<div class="error">{item.error}</div>{/if}
  </div>
{/if}
{#if gaveUp?.error && !item.error}<div class="error">{gaveUp.error}</div>{/if}

<style>
  .run { display: flex; align-items: center; flex-wrap: wrap; gap: 4px 10px; font-size: 12.5px; color: var(--muted); }
  /* A gap between turns: enough to hover (and to scroll to). */
  .run.quiet { min-height: 6px; }
  .trig { font: 600 11px var(--mono); padding: 1px 7px; border-radius: 999px; background: var(--bg3); color: var(--fg); }
  .t-schedule, .t-inject, .t-auto_continue { background: color-mix(in srgb, var(--info) 15%, transparent); }
  .t-steer { background: color-mix(in srgb, var(--warn) 18%, transparent); }
  .st { display: inline-flex; align-items: center; gap: 5px; }
  .o-completed .st { color: var(--ok); }
  .o-failed .st, .o-refusal .st { color: var(--bad); }
  .o-cancelled .st, .o-max_tokens .st { color: var(--warn); }
  .retried { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; }
  .retried.bad { color: var(--bad); }
  .error { flex-basis: 100%; font-size: 12.5px; color: var(--bad); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
