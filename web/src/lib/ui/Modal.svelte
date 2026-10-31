<script lang="ts">
  // A dialog surface with the keyboard rules of every form: Enter runs the primary action
  // (Shift+Enter is a newline), Esc cancels. Fields that own Enter/Esc (autocompletes, dropdowns,
  // title edits) stop propagation first; fields marked data-doc (free-text notes) keep Enter.
  // `page` renders it as a full page instead (Settings' expand button). Bottom sheet on phones.
  import type { Snippet } from 'svelte';

  let {
    label,
    oncancel,
    onprimary,
    wide = false,
    page = false,
    children,
  }: { label: string; oncancel: () => void; onprimary?: () => void; wide?: boolean; page?: boolean; children: Snippet } = $props();

  let box: HTMLElement | undefined = $state();

  function onkey(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    const t = e.target as HTMLElement;
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      oncancel();
      return;
    }
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing || !onprimary) return;
    if (t.tagName === 'BUTTON' || (t as HTMLInputElement).type === 'checkbox' || t.dataset.doc !== undefined) return;
    e.preventDefault();
    e.stopPropagation();
    onprimary();
  }

  $effect(() => {
    if (!box) return;
    const target = box.querySelector<HTMLElement>('[data-autofocus]') ?? box;
    queueMicrotask(() => target.focus());
  });
</script>

{#if page}
  <div class="page" role="dialog" tabindex="-1" aria-label={label} bind:this={box} onkeydown={onkey}>{@render children()}</div>
{:else}
  <div class="bg" role="presentation" onmousedown={(e) => { if (e.target === e.currentTarget) oncancel(); }}>
    <div class="modal" class:wide role="dialog" aria-modal="true" tabindex="-1" aria-label={label} bind:this={box} onkeydown={onkey}>{@render children()}</div>
  </div>
{/if}

<style>
  .bg { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.35); display: grid; place-items: center; z-index: 100; padding: 20px; }
  .modal {
    background: var(--bg2); border: 1px solid var(--line); border-radius: 12px; padding: 18px 20px;
    width: min(640px, 100%); max-height: 90vh; overflow: auto; display: flex; flex-direction: column; gap: 10px; outline: none;
  }
  .modal.wide { width: min(960px, 94vw); height: min(760px, 88vh); max-height: none; overflow: hidden; padding: 20px 24px; }
  .page { grid-area: main; min-height: 0; display: flex; flex-direction: column; gap: 12px; padding: 22px max(28px, calc((100% - 960px) / 2)) 0; background: var(--bg); outline: none; }
  @media (max-width: 760px) {
    .bg { padding: 0; align-items: end; }
    .modal, .modal.wide { width: 100%; height: auto; max-height: 92dvh; border-radius: 16px 16px 0 0; padding: 16px 16px calc(env(safe-area-inset-bottom) + 16px); overflow: auto; }
    .page { padding: calc(env(safe-area-inset-top) + 12px) 16px 0; }
  }
</style>
