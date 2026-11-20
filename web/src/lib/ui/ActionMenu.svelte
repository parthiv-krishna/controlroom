<script lang="ts" module>
  export interface MenuAction {
    label: string;
    run: () => void;
    disabled?: boolean;
    /** A toggle's state (shown as a check). */
    checked?: boolean;
    danger?: boolean;
    /** A shortcut, shown muted after the label. */
    key?: string;
  }
</script>

<script lang="ts">
  // A ⋯ button with a menu of actions. Esc or a click outside closes it; picking an action closes it.
  import { dismissable, fitInView } from './actions';
  import Icon from './Icon.svelte';

  let { label, actions }: { label: string; actions: MenuAction[] } = $props();
  let open = $state(false);
  let anchor: HTMLElement | undefined = $state();

  function pick(a: MenuAction) {
    open = false;
    a.run();
  }
</script>

<span class="am">
  <button bind:this={anchor} class="dots" title={label} aria-label={label} aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}><Icon name="more" /></button>
  {#if open}
    <div class="menu" role="menu" aria-label={label} use:dismissable={{ close: () => (open = false), anchor }} use:fitInView>
      {#each actions as a (a.label)}
        {#if a.checked === undefined}
          <button role="menuitem" class:danger={a.danger} disabled={a.disabled} onclick={() => pick(a)}><span class="chk"></span>{a.label}{#if a.key}<span class="key">{a.key}</span>{/if}</button>
        {:else}
          <button role="menuitemcheckbox" aria-checked={a.checked} disabled={a.disabled} onclick={() => pick(a)}><span class="chk">{a.checked ? '✓' : ''}</span>{a.label}{#if a.key}<span class="key">{a.key}</span>{/if}</button>
        {/if}
      {/each}
    </div>
  {/if}
</span>

<style>
  .am { position: relative; display: inline-flex; }
  .dots { width: 26px; height: 26px; display: grid; place-items: center; background: none; border: 1px solid transparent; border-radius: 7px; color: var(--muted); cursor: pointer; padding: 0; }
  .dots:hover, .dots[aria-expanded='true'] { color: var(--fg); background: var(--bg3); border-color: var(--line); }
  .menu { position: absolute; z-index: 60; top: calc(100% + 4px); right: 0; min-width: 180px; display: flex; flex-direction: column; background: var(--bg2); border: 1px solid var(--line); border-radius: 9px; box-shadow: var(--shadow); padding: 4px; }
  .menu button { display: flex; align-items: center; gap: 6px; background: none; border: none; font: inherit; font-size: 13px; color: var(--fg); padding: 6px 8px; border-radius: 6px; cursor: pointer; text-align: left; white-space: nowrap; }
  .menu button:hover:not(:disabled) { background: var(--hover); }
  .menu button:disabled { opacity: 0.45; cursor: not-allowed; }
  .menu button.danger { color: var(--bad); }
  .chk { width: 14px; flex: none; }
  .key { margin-left: auto; padding-left: 16px; color: var(--muted); font-size: 12px; }
  @media (max-width: 760px) { .menu button { min-height: 44px; font-size: 15px; } }
</style>
