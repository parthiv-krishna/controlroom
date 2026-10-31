<script lang="ts">
  // Hover shows a box and a pencil; click turns the title into an input. Enter or blur saves, Esc
  // cancels. Changing `editKey` (e.g. switching session mid-edit) cancels the edit.
  import Icon from './Icon.svelte';

  let { value, onsave, editKey, heading = false }: { value: string; onsave: (v: string) => void; editKey: string; heading?: boolean } = $props();

  let editing = $state(false);
  let draft = $state('');
  let input: HTMLInputElement | undefined = $state();

  $effect(() => {
    void editKey;
    editing = false;
  });
  $effect(() => {
    if (editing && input) {
      input.focus();
      input.select();
    }
  });

  function start() {
    draft = value;
    editing = true;
  }
  function finish(save: boolean) {
    if (!editing) return;
    editing = false;
    const v = draft.trim();
    if (save && v && v !== value) onsave(v);
  }
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      finish(e.key === 'Enter');
    }
  }
</script>

{#if editing}
  <span class="et editing" class:heading>
    <input bind:this={input} bind:value={draft} size={Math.max(8, draft.length + 1)} spellcheck="false" autocomplete="off" aria-label="Title" onkeydown={onkey} onblur={() => finish(true)} />
  </span>
{:else}
  <button class="et" class:heading title="Rename" onclick={start}>
    <span class="text">{value}</span><span class="pencil"><Icon name="pencil" size={12} /></span>
  </button>
{/if}

<style>
  .et {
    display: inline-flex; align-items: center; gap: 6px; margin-left: -7px; padding: 1px 6px; min-width: 0; max-width: 100%;
    border: 1px solid transparent; border-radius: 7px; background: none; color: inherit; font: inherit; cursor: text; text-align: left;
  }
  .heading { font-size: 20px; font-weight: 600; letter-spacing: -0.015em; }
  .text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pencil { color: var(--muted); opacity: 0; flex: none; }
  .et:hover { border-color: var(--line); background: color-mix(in srgb, var(--bg3) 45%, transparent); }
  .et:hover .pencil, .et:focus-visible .pencil { opacity: 1; }
  .editing { border-color: var(--accent); background: var(--bg2); }
  input { font: inherit; color: inherit; background: none; border: none; outline: none; padding: 0; width: auto; min-width: 6ch; max-width: 60vw; }
  @media (hover: none) { .pencil { opacity: 0.55; } }
</style>
