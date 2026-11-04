<script lang="ts">
  // The phone's extra keys under a terminal: Esc, Tab, a sticky Ctrl (applies to the next typed
  // character), arrows and a few symbols touch keyboards hide. Pressing one keeps the terminal
  // focused, so the soft keyboard stays up.
  import { EXTRA_KEYS } from './keys';

  let { send, ctrl = $bindable(false) }: { send: (seq: string) => void; ctrl?: boolean } = $props();
</script>

<div class="xkeys" role="toolbar" aria-label="Extra keys">
  {#each EXTRA_KEYS as k (k.name)}
    {#if k.seq}
      <button onpointerdown={(e) => e.preventDefault()} onclick={() => send(k.seq)}>{k.label}</button>
    {:else}
      <button class:on={ctrl} aria-pressed={ctrl} onpointerdown={(e) => e.preventDefault()} onclick={() => (ctrl = !ctrl)}>{k.label}</button>
    {/if}
  {/each}
</div>

<style>
  .xkeys { display: flex; gap: 4px; padding: 6px; overflow-x: auto; background: #1f2228; border-top: 1px solid #2c3038; flex: none; padding-bottom: calc(6px + env(safe-area-inset-bottom)); }
  button { flex: 1 0 auto; min-width: 40px; padding: 9px 8px; border-radius: 7px; border: 1px solid #3a3f4b; background: #2a2e36; color: #d7dae0; font: 13px var(--mono); }
  button.on { background: #5b8def; border-color: #5b8def; color: #fff; }
</style>
