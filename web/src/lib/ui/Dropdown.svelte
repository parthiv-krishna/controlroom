<script lang="ts" module>
  export interface Option {
    value: string;
    label: string;
    hint?: string;
    disabled?: boolean;
  }
</script>

<script lang="ts">
  // An app-styled select (no native selects): Enter/Space/↓ opens, ↑↓ move, Enter picks, Esc closes.
  // `icon` renders a leading snippet per option.
  import type { Snippet } from 'svelte';
  import { dismissable } from './actions';

  let {
    value,
    options,
    onchange,
    label,
    placeholder = 'Select…',
    small = false,
    disabled = false,
    id,
    icon,
  }: {
    value: string;
    options: Option[];
    onchange: (value: string) => void;
    label: string;
    placeholder?: string;
    small?: boolean;
    disabled?: boolean;
    id?: string;
    icon?: Snippet<[Option]>;
  } = $props();

  let open = $state(false);
  let button: HTMLButtonElement | undefined = $state();
  let hi = $state(0);
  const current = $derived(options.find((o) => o.value === value));

  function toggle() {
    open = !open;
    hi = Math.max(0, options.findIndex((o) => o.value === value));
  }
  function pick(o: Option | undefined) {
    if (!o || o.disabled) return;
    open = false;
    if (o.value !== value) onchange(o.value);
  }
  function onkey(e: KeyboardEvent) {
    if (!open) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        toggle();
      }
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      hi = Math.max(0, Math.min(options.length - 1, hi + (e.key === 'ArrowDown' ? 1 : -1)));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      pick(options[hi]);
    }
  }
</script>

<span class="dd" class:sm={small} class:open>
  <button type="button" class="dd-btn" bind:this={button} {id} {disabled} aria-label={label} aria-haspopup="listbox" aria-expanded={open} onclick={toggle} onkeydown={onkey}>
    {#if current}
      {#if icon}{@render icon(current)}{/if}<span class="dd-l">{current.label}</span>
    {:else}
      <span class="dd-l dd-ph">{placeholder}</span>
    {/if}
    <span class="dd-caret">▾</span>
  </button>
  {#if open}
    <div class="dd-menu" role="listbox" aria-label={label} use:dismissable={{ close: () => (open = false), anchor: button }}>
      {#each options as o, i (o.value)}
        <button type="button" role="option" aria-selected={o.value === value} class="dd-opt" class:on={o.value === value} class:hi={i === hi} disabled={o.disabled} onclick={() => pick(o)} onmousemove={() => (hi = i)}>
          {#if icon}{@render icon(o)}{/if}<span class="dd-l">{o.label}</span>{#if o.hint}<span class="dd-hint">{o.hint}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
</span>

<style>
  .dd { position: relative; display: inline-flex; max-width: 100%; vertical-align: middle; }
  .dd-btn {
    display: inline-flex; align-items: center; gap: 7px; width: 100%; min-width: 0;
    background: var(--bg2); color: var(--fg); border: 1px solid var(--line); border-radius: 8px;
    padding: 6px 9px; font: inherit; font-size: 13.5px; cursor: pointer; text-align: left;
  }
  .dd-btn:hover:not(:disabled) { border-color: var(--muted); }
  .dd-btn:disabled { opacity: 0.55; cursor: default; }
  .open .dd-btn { border-color: var(--accent); }
  .sm .dd-btn { padding: 3px 8px; font-size: 12.5px; border-radius: 7px; }
  .dd-l { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .dd-ph { color: var(--muted); }
  .dd-caret { color: var(--muted); font-size: 10px; flex: none; }
  .dd-menu {
    position: absolute; top: calc(100% + 4px); left: 0; min-width: 100%; width: max-content; max-width: 380px;
    max-height: 300px; overflow-y: auto; z-index: 130; background: var(--bg2); border: 1px solid var(--line);
    border-radius: 10px; box-shadow: var(--shadow); padding: 4px; display: flex; flex-direction: column;
  }
  .dd-opt {
    display: flex; align-items: center; gap: 8px; background: none; border: none; color: var(--fg); font: inherit;
    font-size: 13px; text-align: left; padding: 6px 9px; border-radius: 6px; cursor: pointer;
  }
  .dd-opt.hi:not(:disabled) { background: var(--bg3); }
  .dd-opt.on { font-weight: 600; }
  .dd-opt:disabled { opacity: 0.45; cursor: default; }
  .dd-hint { margin-left: auto; padding-left: 12px; font-size: 11.5px; color: var(--muted); font-weight: 400; }
  @media (max-width: 760px) {
    .dd-menu { position: fixed; left: 0; right: 0; bottom: 0; top: auto; max-width: none; width: auto; border-radius: 16px 16px 0 0; max-height: 60dvh; padding-bottom: calc(env(safe-area-inset-bottom) + 8px); }
    .dd-opt { min-height: 44px; font-size: 15px; }
  }
</style>
