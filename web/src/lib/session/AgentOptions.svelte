<script lang="ts">
  // The agent's neutral options (model, thinking level, …), from the summary.
  // Changing one calls session.set_option, which needs a live, ready agent (`agent_live`: also
  // after a failed turn); a select stays disabled until the agent reported its choices. Models of a
  // provider with several accounts (Settings › Models) are labeled with their account. The model
  // choices are the session's profile's accounts only (agentd already limits what pi offers; this
  // keeps a stale list from offering more). The current value always stays.
  import { useApp } from '../app/app.svelte';
  import { modelLabel, modelProvider } from '../model/accounts';
  import { admits } from '../model/profiles';
  import { onSession } from '../app/app.svelte';
  import type { FeedSession } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const editable = $derived(app.client.online && s.agent_live);
  $effect(() => app.accounts.ensure());
  const label = (id: string, value: string, label: string) => (id === 'model' ? modelLabel(app.accounts.list, value, label) : label);
  const policy = $derived(app.profiles.find((p) => p.id === s.profile_id)?.models ?? null);
  /** A choice the profile allows: model choices of accounts outside its policy are left out. */
  function allowed(id: string, value: string, current: string): boolean {
    const provider = id === 'model' ? modelProvider(value) : null;
    return !policy || !provider || value === current || admits(policy, provider);
  }
</script>

<span class="opts">
  {#each s.options as o (o.id)}
    {#if o.kind === 'select'}
      {@const ready = editable && o.choices.length > 0}
      <span class="opt" class:locked={!ready} title={ready ? o.label : `${o.label} (${editable ? 'choices not loaded yet' : 'needs a running agent'})`}>
        <Dropdown small label={o.label} value={o.value} disabled={!ready} options={o.choices.filter((c) => allowed(o.id, c.value, o.value)).map((c) => ({ value: c.value, label: label(o.id, c.value, c.label) }))} onchange={(value) => app.client.call('session.set_option', { ...onSession(s), option_id: o.id, value })} />
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
