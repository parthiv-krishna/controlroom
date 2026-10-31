<script lang="ts">
  import { ICONS, type IconName } from './icons';

  let { name, size = 14, label }: { name: IconName; size?: number; label?: string } = $props();
</script>

<svg class="icon" viewBox="0 0 16 16" width={size} height={size} role={label ? 'img' : undefined} aria-label={label} aria-hidden={label ? undefined : 'true'}>
  {#each ICONS[name] as p (p.d)}
    {#if p.fill}
      <path d={p.d} fill="currentColor" />
    {:else}
      <path d={p.d} fill="none" stroke="currentColor" stroke-width={p.width ?? 1.5} stroke-linecap="round" stroke-linejoin="round" />
    {/if}
  {/each}
</svg>

<style>
  .icon {
    display: block;
    flex: none;
  }
</style>
