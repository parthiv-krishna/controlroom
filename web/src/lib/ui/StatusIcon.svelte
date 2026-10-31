<script lang="ts">
  // One icon language for sessions and machines (spec §13): pulsing orb working, flag needs you
  // (outline: a non-blocking question), check complete and unread, nothing when complete and read,
  // cross failed or stopped, clock idle, broken chain disconnected, box archived.
  import type { StatusKind } from '../model/status';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';

  let { kind, title }: { kind: StatusKind; title?: string } = $props();

  const ICON: Record<Exclude<StatusKind, 'working' | 'done'>, IconName> = {
    attention: 'flag',
    asking: 'flagOutline',
    complete: 'check',
    stopped: 'cross',
    idle: 'clock',
    offline: 'chain',
    archived: 'archive',
  };
</script>

<span class="si si-{kind}" {title}>
  {#if kind === 'working'}<i class="orb"></i>{:else if kind !== 'done'}<Icon name={ICON[kind]} size={13} />{/if}
</span>

<style>
  .si {
    display: inline-grid;
    place-items: center;
    width: 14px;
    height: 14px;
    flex: none;
  }
  .si-working { color: var(--info); }
  .si-attention, .si-asking { color: var(--warn); }
  .si-complete { color: var(--ok); }
  .si-stopped { color: var(--bad); }
  .si-idle, .si-offline { color: var(--muted); }
  .si-archived { color: var(--fg); }
  .orb {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentColor;
    animation: orb 1.4s ease-out infinite;
  }
  @keyframes orb {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--info) 60%, transparent); opacity: 1; }
    70% { box-shadow: 0 0 0 5px transparent; opacity: 0.55; }
    100% { box-shadow: 0 0 0 0 transparent; opacity: 1; }
  }
</style>
