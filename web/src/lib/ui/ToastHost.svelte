<script lang="ts">
  import { useApp } from '../app/app.svelte';

  const app = useApp();
</script>

<div class="toasts" aria-live="polite">
  {#each app.toasts.list as t (t.id)}
    <div class="toast {t.level}" role="status">
      <span>{t.text}</span>
      {#if t.session}
        {@const ref = t.session}
        <button class="btn xs" onclick={() => app.open(ref)}>Open</button>
      {/if}
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 16px; bottom: 16px; display: flex; flex-direction: column; gap: 8px; z-index: 400; pointer-events: none; }
  .toast {
    pointer-events: auto; display: flex; align-items: center; gap: 10px; background: var(--bg2); color: var(--fg);
    border: 1px solid var(--line); border-left: 4px solid var(--info); padding: 9px 12px; border-radius: 8px;
    max-width: 380px; box-shadow: var(--shadow); font-size: 13px;
  }
  .warn { border-left-color: var(--warn); }
  .error { border-left-color: var(--bad); }
  .ok { border-left-color: var(--ok); }
  @media (max-width: 760px) {
    .toasts { left: 12px; right: 12px; top: calc(env(safe-area-inset-top) + 8px); bottom: auto; }
    .toast { max-width: none; }
  }
</style>
