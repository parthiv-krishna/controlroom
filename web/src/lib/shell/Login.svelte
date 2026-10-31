<script lang="ts">
  // Not signed in (or this device was removed). With a login URL (OIDC) the browser goes there.
  let { loginUrl }: { loginUrl: string | null } = $props();
  $effect(() => {
    if (loginUrl) location.assign(loginUrl);
  });
</script>

<div class="center" role="status">
  {#if loginUrl}
    <span class="spin"></span><b>Signing in…</b>
  {:else}
    <div><b>Not signed in</b><div class="small muted">Open the sign-in link printed by the server.</div></div>
  {/if}
</div>

<style>
  .center { position: fixed; inset: 0; display: flex; align-items: center; justify-content: center; gap: 12px; padding: 24px; }
</style>
