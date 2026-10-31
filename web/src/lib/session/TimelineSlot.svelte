<script lang="ts">
  // The timeline body. P4 renders the session-stream store here (one component per item kind,
  // windowed). Until then the slot shows the session's preview and its pending requests (which P4
  // renders inline as `request` items), so every request is answerable.
  import type { MachineSession } from '../protocol';
  import RequestCard from './RequestCard.svelte';

  let { s }: { s: MachineSession } = $props();
  const pending = $derived(s.requests.filter((r) => r.state === 'pending'));
</script>

<div class="timeline" data-pane="session" tabindex="-1" aria-label="Timeline">
  <div class="placeholder" data-testid="timeline-slot">
    {#if s.preview}<p class="preview">{s.preview}</p>{/if}
    {#if s.state === 'starting'}<p class="muted"><span class="spin"></span> starting agent…</p>{/if}
  </div>
  {#each pending as req (req.id)}<RequestCard {req} {s} />{/each}
</div>

<style>
  .timeline { flex: 1; overflow-y: auto; padding: 16px 40px; max-width: 900px; width: 100%; margin: 0 auto; display: flex; flex-direction: column; gap: 16px; font-size: 15px; line-height: 1.6; min-height: 0; outline: none; }
  .placeholder { flex: 1; display: flex; flex-direction: column; justify-content: flex-end; }
  .preview { color: var(--fg); margin: 0; }
  @media (max-width: 760px) { .timeline { padding: 12px 16px; } }
</style>
