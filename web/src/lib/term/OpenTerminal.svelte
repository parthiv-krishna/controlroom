<script lang="ts">
  // The shell of a `terminal_input` request, as a link: shows the session (via `open`), opens its
  // terminal dock at the waiting shell and focuses it, so the user types the answer there.
  import type { SessionId } from '../protocol';
  import { docks } from './dock.svelte';

  let { session, shell, open }: { session: SessionId; shell: string; open?: () => void } = $props();

  function go() {
    open?.();
    docks.focusShell(session, shell);
  }
</script>

<button class="linkish shell" aria-label="Open terminal “{shell}”" title="Open its terminal" onclick={go}>{shell}</button>

<style>
  .shell { font-weight: 600; }
</style>
