<script lang="ts">
  // Machine status at the top right: one button that opens Settings › Machines. ✓ connected (a
  // subtle ↑ when a compatible update is available), broken chain unreachable, orb connecting, the
  // flag only when the user must act (incompatible, action required).
  import { useApp } from '../app/app.svelte';
  import { ago, connKind, connLabel } from '../model/status';
  import type { Machine } from '../protocol';
  import StatusIcon from '../ui/StatusIcon.svelte';

  const app = useApp();
  const update = (m: Machine) => m.connectivity.state === 'connected' && m.update_available;
  function title(m: Machine): string {
    switch (m.connectivity.state) {
      case 'connected':
        return update(m) ? `${m.name} · Update available: ${m.version ?? '?'} → ${app.client.hello?.server_version ?? 'newer'}` : `${m.name} · connected · controlroomd ${m.version ?? ''}`;
      case 'unreachable':
        return `${m.name} · unreachable · last seen ${ago(m.last_seen)}`;
      case 'incompatible':
        return `${m.name} · daemon ${m.version ?? ''} is outdated: upgrade required`;
      default:
        return `${m.name} · ${connLabel(m)}`;
    }
  }
</script>

<button class="machines" title="Machines" aria-label="Machines" onclick={() => app.openSettings('machines')}>
  {#each app.client.feed.machineList as m (m.id)}
    <span class="m" class:act={m.connectivity.state !== 'connected'} title={title(m)}>
      <StatusIcon kind={connKind(m)} />{#if update(m)}<span class="up" aria-label="update available">↑</span>{/if}<span class="name">{m.name}</span>
    </span>
  {/each}
</button>

<style>
  .machines { display: flex; align-items: center; gap: 2px; background: none; border: 1px solid transparent; border-radius: 9px; padding: 3px 6px; font: inherit; color: inherit; cursor: pointer; flex: none; }
  .machines:hover { background: var(--bg3); border-color: var(--line); }
  .m { display: inline-flex; align-items: center; gap: 5px; font-size: 12.5px; color: var(--muted); padding: 3px 6px; }
  .m.act { color: var(--fg); }
  .machines:hover .m { color: var(--fg); }
  .up { font-size: 10px; font-weight: 700; color: var(--info); margin: 0 1px 0 -3px; vertical-align: super; line-height: 0; }
  @container top (max-width: 760px) { .name { display: none; } }
</style>
