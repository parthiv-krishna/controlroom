<script lang="ts">
  // A profile's chip: its color, name and a lock when private. Callers show it only with several
  // profiles. With `onclick` it is a button.
  import { profileLabel } from '../model/profiles';
  import type { Profile } from '../protocol';
  import Icon from '../ui/Icon.svelte';

  let { profile, onclick, title }: { profile: Profile; onclick?: () => void; title?: string } = $props();
  const label = $derived(profileLabel(profile));
</script>

{#snippet body()}
  <span class="dot" aria-hidden="true"></span><span class="nm">{profile.name}</span>{#if profile.private}<Icon name="lock" size={11} label="private" />{/if}
{/snippet}

{#if onclick}
  <button class="pchip" style:--pf={profile.color} title={title ?? label} aria-label={title ?? label} {onclick}>{@render body()}</button>
{:else}
  <span class="pchip" style:--pf={profile.color} title={title ?? label} role="img" aria-label={label}>{@render body()}</span>
{/if}

<style>
  .pchip {
    display: inline-flex; align-items: center; gap: 5px; flex: none; max-width: 180px; font: inherit; font-size: 11.5px; line-height: 1.5;
    padding: 0 7px 0 6px; border-radius: 999px; border: 1px solid color-mix(in srgb, var(--pf) 45%, var(--line));
    background: color-mix(in srgb, var(--pf) 10%, transparent); color: color-mix(in srgb, var(--pf) 55%, var(--fg));
  }
  button.pchip { cursor: pointer; }
  button.pchip:hover { background: color-mix(in srgb, var(--pf) 20%, transparent); }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--pf); flex: none; }
  .nm { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
</style>
