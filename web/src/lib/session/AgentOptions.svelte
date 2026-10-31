<script lang="ts">
  // The agent's neutral options (model, thinking level, …) and the context meter, from the summary.
  // Changing one calls session.set_option, which needs a live agent.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import type { MachineSession } from '../protocol';
  import ContextMeter from '../ui/ContextMeter.svelte';
  import Dropdown from '../ui/Dropdown.svelte';

  let { s }: { s: MachineSession } = $props();
  const app = useApp();
  const agentLive = $derived(['idle', 'working', 'awaiting_permission', 'awaiting_input'].includes(s.state));
  const editable = $derived(app.client.online && !s.stale && agentLive);
</script>

<span class="opts">
  {#each s.options as o (o.id)}
    {#if o.kind === 'select'}
      <span class="opt" class:locked={!editable} title={editable ? o.label : `${o.label} (needs a running agent)`}>
        <Dropdown small label={o.label} value={o.value} disabled={!editable} options={o.choices.map((c) => ({ value: c.value, label: c.label }))} onchange={(value) => app.client.call('session.set_option', { ...onSession(s), option_id: o.id, value })} />
      </span>
    {:else if o.kind === 'bool'}
      <label class="small check"><input type="checkbox" checked={o.value} disabled={!editable} onchange={(e) => app.client.call('session.set_option', { ...onSession(s), option_id: o.id, value: e.currentTarget.checked })} /> {o.label}</label>
    {/if}
  {/each}
  {#if s.context}<ContextMeter usage={s.context} />{/if}
</span>

<style>
  .opts { display: inline-flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .opt.locked { opacity: 0.7; }
  .check { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); }
</style>
