<script lang="ts">
  // Renders the in-app confirm / ask dialog (Enter confirms, Esc cancels).
  import { useApp } from '../app/app.svelte';
  import { autofocus } from './actions';

  const dialogs = useApp().dialogs;
  const d = $derived(dialogs.current);
  let value = $state('');
  $effect(() => {
    value = d?.value ?? '';
  });

  const yes = () => dialogs.close(d?.kind === 'ask' ? value : true);
  const no = () => dialogs.close(d?.kind === 'ask' ? null : false);
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      no();
    } else if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      yes();
    }
  }
</script>

{#if d}
  <div class="bg" role="presentation" onmousedown={(e) => { if (e.target === e.currentTarget) no(); }} onkeydown={onkey}>
    <div class="dlg" role="alertdialog" aria-modal="true" aria-label={d.title}>
      <h4>{d.title}</h4>
      {#if d.body}<p class="body">{d.body}</p>{/if}
      {#if d.kind === 'ask'}
        {#if d.multiline}
          <textarea rows="4" placeholder={d.placeholder ?? ''} aria-label={d.title} bind:value use:autofocus></textarea>
        {:else}
          <input placeholder={d.placeholder ?? ''} aria-label={d.title} autocomplete="off" spellcheck="false" bind:value use:autofocus />
        {/if}
      {/if}
      {#if d.hint}<div class="hint">{d.hint}</div>{/if}
      <div class="acts">
        <button class="btn" onclick={no}><span class="kbd">Esc</span> Cancel</button>
        {#if d.kind === 'confirm'}
          <button class="btn {d.danger ? 'danger' : 'primary'}" onclick={yes} use:autofocus><span class="kbd">↵</span> {d.ok ?? 'OK'}</button>
        {:else}
          <button class="btn {d.danger ? 'danger' : 'primary'}" onclick={yes}><span class="kbd">↵</span> {d.ok ?? 'OK'}</button>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .bg { position: fixed; inset: 0; z-index: 300; background: rgba(0, 0, 0, 0.32); display: grid; place-items: center; padding: 20px; }
  .dlg {
    width: min(440px, 100%); background: var(--bg2); border: 1px solid var(--line); border-radius: 14px;
    box-shadow: var(--shadow); padding: 18px 18px 14px; display: flex; flex-direction: column; gap: 10px;
  }
  h4 { font-size: 15.5px; }
  .body { margin: 0; color: var(--muted); font-size: 13.5px; }
  .hint { font-size: 12px; color: var(--muted); }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
  @media (max-width: 760px) {
    .bg { align-items: end; padding: 0; }
    .dlg { width: 100%; border-radius: 16px 16px 0 0; padding-bottom: calc(env(safe-area-inset-bottom) + 16px); }
  }
</style>
