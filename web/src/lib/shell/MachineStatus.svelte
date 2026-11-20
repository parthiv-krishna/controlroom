<script lang="ts">
  // Machine status in the rail, above Settings: one button that opens Settings › Machines. Its icon
  // is the worst machine's (flag when the user must act, broken chain unreachable, orb connecting,
  // ✓ all connected; a subtle ↑ when a compatible update is available), with the count of machines
  // that need a look. The tooltip names them, one per line, and counts the healthy rest.
  import { useApp } from '../app/app.svelte';
  import { ago, connKind, connLabel, type StatusKind } from '../model/status';
  import type { Machine } from '../protocol';
  import StatusIcon from '../ui/StatusIcon.svelte';

  const app = useApp();
  const update = (m: Machine) => m.connectivity.state === 'connected' && m.update_available;
  const healthy = (m: Machine) => m.connectivity.state === 'connected' && !m.update_available;
  const machines = $derived(app.client.feed.machineList);
  const flagged = $derived(machines.filter((m) => !healthy(m)));
  const ok = $derived(machines.length - flagged.length);
  const RANK: StatusKind[] = ['attention', 'offline', 'working', 'complete'];
  const worst = $derived(flagged.map(connKind).sort((a, b) => RANK.indexOf(a) - RANK.indexOf(b))[0] ?? 'complete');
  function line(m: Machine): string {
    switch (m.connectivity.state) {
      case 'connected':
        return `${m.name}: update available (${m.version ?? '?'} → ${app.client.hello?.server_version ?? 'newer'})`;
      case 'unreachable':
        return `${m.name}: unreachable, last seen ${ago(m.last_seen)}`;
      case 'incompatible':
        return `${m.name}: daemon ${m.version ?? ''} is outdated, upgrade required`;
      default:
        return `${m.name}: ${connLabel(m)}`;
    }
  }
  const title = $derived([...flagged.map(line), ok ? `${ok} connected` : ''].filter(Boolean).join('\n'));
</script>

{#if machines.length}
  <button class="machines" class:act={flagged.length > 0} {title} aria-label="Machines" onclick={() => app.openSettings('machines')}>
    <StatusIcon kind={worst} />{#if flagged.some(update) && worst === 'complete'}<span class="up" aria-hidden="true">↑</span>{/if}
    {#if flagged.length}<span class="n" data-flagged>{flagged.length}</span>{/if}
  </button>
{/if}

<style>
  .machines {
    position: relative; width: 36px; height: 36px; flex: none; display: grid; place-items: center; background: none;
    border: 1px solid var(--line); border-radius: 10px; color: var(--muted); cursor: pointer; padding: 0; font: inherit;
  }
  .machines:hover, .machines.act { color: var(--fg); }
  .machines:hover { background: var(--bg3); }
  .n { position: absolute; top: -5px; right: -5px; min-width: 16px; height: 16px; padding: 0 4px; border-radius: 999px; background: var(--warn); color: #3a2a00; font-size: 10px; font-weight: 700; display: grid; place-items: center; }
  .up { position: absolute; top: 3px; right: 5px; font-size: 10px; font-weight: 700; color: var(--info); }
</style>
