<script lang="ts">
  // The agent's neutral options (model, thinking level, …), from the summary.
  // Changing one calls session.set_option, which needs a live, ready agent (`agent_live`: also
  // after a failed turn); a select stays disabled until the agent reported its choices.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import type { FeedSession } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const editable = $derived(app.client.online && s.agent_live);
</script>

<span class="opts">
  {#each s.options as o (o.id)}
    {#if o.kind === 'select'}
      {@const ready = editable && o.choices.length > 0}
      <span class="opt" class:locked={!ready} title={ready ? o.label : `${o.label} (${editable ? 'choices not loaded yet' : 'needs a running agent'})`}>
        <Dropdown small label={o.label} value={o.value} disabled={!ready} options={o.choices.map((c) => ({ value: c.value, label: c.label }))} onchange={(value) => app.client.call('session.set_option', { ...onSession(s), option_id: o.id, value })} />
      </span>
    {:else if o.kind === 'bool'}
      <label class="small check"><input type="checkbox" checked={o.value} disabled={!editable} onchange={(e) => app.client.call('session.set_option', { ...onSession(s), option_id: o.id, value: e.currentTarget.checked })} /> {o.label}</label>
    {/if}
  {/each}
</span>

<style>
  .opts { display: inline-flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .opt.locked { opacity: 0.7; }
  .check { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); }
</style>
