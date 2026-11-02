<script lang="ts">
  // A run marker: what started it (device, queue, steer, injection, auto-continue, retry,
  // extension, schedule or unknown), whether it is running, and how it ended.
  import { useApp } from '../../app/app.svelte';
  import { ago } from '../../model/status';
  import type { DeviceRef, RunTrigger } from '../../protocol';
  import type { ItemOf } from '../../state/timeline.svelte';

  let { item }: { item: ItemOf<'run'> } = $props();
  const app = useApp();
  const OUTCOME = { completed: 'done', cancelled: 'stopped', failed: 'failed', max_tokens: 'max tokens', refusal: 'refused', unknown: 'ended' } as const;
  const INJECT = { user: 'user', job: 'job', child: 'subagent', message: 'message', answer: 'answer', todo: 'todo edit', continue: 'continue', unknown: 'injected' } as const;

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
</script>

<div class="run o-{item.outcome ?? 'none'}" class:running={item.state === 'running'}>
  <span class="trig t-{item.trigger.kind}" title={t.title}>{t.label}</span>
  <span class="st">
    {#if item.state === 'running'}<span class="spin"></span> running{:else}{OUTCOME[item.outcome ?? 'unknown']}{/if}
  </span>
  <span class="when muted" title={new Date(item.ts).toLocaleString()}>{ago(item.ts)}</span>
  {#if item.error}<div class="error">{item.error}</div>{/if}
</div>

<style>
  .run { display: flex; align-items: center; flex-wrap: wrap; gap: 4px 10px; font-size: 12.5px; color: var(--muted); border-top: 1px solid var(--line); padding-top: 6px; }
  .trig { font: 600 11px var(--mono); padding: 1px 7px; border-radius: 999px; background: var(--bg3); color: var(--fg); }
  .t-schedule, .t-inject, .t-auto_continue { background: color-mix(in srgb, var(--info) 15%, transparent); }
  .t-steer { background: color-mix(in srgb, var(--warn) 18%, transparent); }
  .st { display: inline-flex; align-items: center; gap: 5px; }
  .o-completed .st { color: var(--ok); }
  .o-failed .st, .o-refusal .st { color: var(--bad); }
  .o-cancelled .st, .o-max_tokens .st { color: var(--warn); }
  .when { font-size: 11.5px; }
  .error { flex-basis: 100%; color: var(--bad); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
