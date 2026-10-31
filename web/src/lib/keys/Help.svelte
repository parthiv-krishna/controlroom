<script lang="ts">
  import { useApp } from '../app/app.svelte';
  import Modal from '../ui/Modal.svelte';
  import { SHORTCUTS } from './shortcuts';

  const app = useApp();
  const close = () => (app.view.help = false);
</script>

{#if app.view.help}
  <Modal label="Keyboard" oncancel={close} onprimary={close}>
    <h3>Keyboard</h3>
    <div class="small muted">Vim-forward; change it in <button class="linkish" onclick={() => { close(); app.openSettings('keyboard'); }}>Settings › Keyboard</button>.</div>
    <table class="small">
      <tbody>
        {#each SHORTCUTS as [k, what] (k)}<tr><td><span class="key">{k}</span></td><td>{what}</td></tr>{/each}
      </tbody>
    </table>
  </Modal>
{/if}

<style>
  table { border-collapse: collapse; }
  td { padding: 4px 12px 4px 0; border-bottom: 1px solid var(--line); vertical-align: top; }
  .key { font-family: var(--mono); white-space: nowrap; }
</style>
