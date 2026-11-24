<script lang="ts">
  // A profile: name, color and (new ones only) private. A new profile carries on with `then` (the
  // fresh-install flow: the action that needed a profile continues in it). Privacy of an existing
  // profile is the picker's ⋯ toggle (it has rules: listed accounts, no shared machines).
  import { useApp } from '../app/app.svelte';
  import { describeError } from '../api/errors';
  import { SWATCHES, nextSwatch } from '../model/colors';
  import type { Profile, ProfileId } from '../protocol';
  import Modal from '../ui/Modal.svelte';

  let { id, then }: { id: ProfileId | null; then?: (p: Profile) => void } = $props();
  const app = useApp();
  const close = () => (app.view.modal = null);

  // svelte-ignore state_referenced_locally
  const base = id ? app.client.feed.profiles.get(id) : undefined;
  let name = $state(base?.name ?? '');
  let color = $state(base?.color ?? nextSwatch(app.profiles.map((p) => p.color)));
  let isPrivate = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  const first = $derived(!id && !app.profiles.length);

  async function save() {
    if (busy) return;
    const n = name.trim();
    if (!n) {
      error = 'Give the profile a name';
      return;
    }
    busy = true;
    error = null;
    try {
      const p = id
        ? await app.client.request('profiles.update', { profile_id: id, name: n, color })
        : await app.client.request('profiles.create', { name: n, color, private: isPrivate });
      // Props are gone once the dialog closes.
      const next = then;
      close();
      next?.(p);
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal label={id ? 'Profile' : 'New profile'} oncancel={close} onprimary={save}>
  <h3>{id ? 'Edit profile' : 'New profile'}</h3>
  {#if first}<p class="small muted">Machines, projects and sessions belong to a profile.</p>{/if}
  <label class="field"><span>Name</span><input bind:value={name} maxlength="80" data-autofocus /></label>
  <div class="field">
    <span>Color</span>
    <div class="swatches" role="radiogroup" aria-label="Color">
      {#each SWATCHES as c (c)}<button class="swatch" class:on={color === c} style:--sw={c} role="radio" aria-checked={color === c} aria-label={c} onclick={() => (color = c)}></button>{/each}
      <label class="custom" title="Another color"><input type="color" aria-label="Another color" bind:value={color} /></label>
    </div>
  </div>
  {#if !id}
    <label class="check" title="Push names only the profile; its machines aren't shared; its model accounts are listed one by one">
      <input type="checkbox" bind:checked={isPrivate} /> Private
    </label>
  {/if}
  {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
  <div class="acts">
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={busy || !app.client.online} onclick={save}><span class="kbd">↵</span> {id ? 'Save' : 'Create'}</button>
  </div>
</Modal>

<style>
  p { margin: 0; }
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  .swatches { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; padding: 6px 0; }
  .swatch { width: 22px; height: 22px; border-radius: 50%; border: 2px solid transparent; background: var(--sw); cursor: pointer; padding: 0; box-shadow: inset 0 0 0 2px var(--bg2); }
  .swatch.on { border-color: var(--fg); }
  .custom input { width: 28px; height: 24px; padding: 0; border: 1px dashed var(--line); border-radius: 6px; background: none; cursor: pointer; }
  .check { display: flex; align-items: center; gap: 8px; font-size: 13.5px; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
</style>
