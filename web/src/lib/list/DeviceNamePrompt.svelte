<script lang="ts">
  // First load: an unobtrusive inline "Name this device" bar, pre-filled with a guess from the user
  // agent. Skipping keeps the guess.
  import { useApp } from '../app/app.svelte';

  const client = useApp().client;
  // svelte-ignore state_referenced_locally
  let name = $state(client.device.label);
  const save = () => (name.trim() ? void client.nameDevice(name.trim()) : client.skipNaming());
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault();
      save();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      client.skipNaming();
    }
  }
</script>

<div class="devname" role="region" aria-label="Name this device">
  <div><b>Name this device</b> <span class="muted small">so you can tell it apart in Settings › Devices</span></div>
  <div class="row">
    <input aria-label="Device name" bind:value={name} maxlength="60" onkeydown={onkey} />
    <button class="btn sm primary" onclick={save}>Save</button>
    <button class="btn sm ghost" title="Keep “{client.device.label}”" onclick={() => client.skipNaming()}>Skip</button>
  </div>
</div>

<style>
  .devname { margin: 10px 12px 0; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--bg2); display: flex; flex-direction: column; gap: 6px; }
  .row { display: flex; gap: 6px; align-items: center; }
  input { flex: 1; min-width: 0; padding: 4px 8px; font-size: 13px; }
</style>
