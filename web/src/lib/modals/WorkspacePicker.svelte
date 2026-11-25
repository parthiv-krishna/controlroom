<script lang="ts">
  // A workspace to create: a machine of the profile (only connected ones can be picked) and a
  // directory browsed on it (`fs.list`), the profile's recent directories there first. Renders the
  // machine, a colon and the directory as siblings, so the caller lays them out (a grid row).
  // `n` numbers the labels ("Machine 2", "Directory 2") when there are several.
  import { useApp } from '../app/app.svelte';
  import { connKind, connLabel, isLive } from '../model/status';
  import { recentDirs } from '../model/workspaces';
  import type { MachineId, ProfileId } from '../protocol';
  import Dropdown, { type Option } from '../ui/Dropdown.svelte';
  import PathCombo from '../ui/PathCombo.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';

  let {
    machine = $bindable(),
    path = $bindable(),
    profile,
    n,
    autofocus = false,
  }: { machine: MachineId; path: string; profile: ProfileId | null; n?: number; autofocus?: boolean } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const scoped = $derived(app.ctxOf(profile));
  const suffix = $derived(n === undefined ? '' : ` ${n}`);
  const key = $derived(n === undefined ? '' : `-${n - 1}`);
  const options = $derived<Option[]>(scoped.machineList.map((m) => ({ value: m.id, label: m.name, disabled: !isLive(m), hint: isLive(m) ? undefined : connLabel(m) })));
  const recent = $derived(recentDirs(scoped.sessions, machine, feed.machines.get(machine)?.user ?? ''));
</script>

{#snippet machineIcon(o: Option)}
  {@const m = feed.machines.get(o.value)}
  {#if m}<StatusIcon kind={connKind(m)} />{/if}
{/snippet}

<Dropdown id="ws-machine{key}" label="Machine{suffix}" value={machine} {options} icon={machineIcon} onchange={(v) => (machine = v)} />
<span class="colon">:</span>
<PathCombo id="ws-path{key}" label="Directory{suffix}" {autofocus} bind:value={path} machineId={machine} {recent} />

<style>
  .colon { font: 13px var(--mono); color: var(--muted); }
  @media (max-width: 760px) { .colon { display: none; } }
</style>
