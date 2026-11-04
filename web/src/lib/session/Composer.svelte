<script lang="ts">
  // The composer: Enter sends (queues while the agent is busy), Shift+Enter is a newline, Esc
  // stops a running turn. Disabled while offline, on a disconnected machine or when archived; the
  // draft is kept until the call succeeds.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { textContent } from '../model/misc';
  import { busy } from '../model/status';
  import type { FeedSession } from '../protocol';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const key = $derived(s.id);
  const isBusy = $derived(busy(s));
  const disabled = $derived(!app.client.online || s.state === 'closed');
  const placeholder = $derived(
    s.state === 'closed' ? 'Archived' : !app.client.online ? 'Not connected' : isBusy ? 'Queue a message…' : s.state === 'dormant' && !s.resumable ? 'Start fresh to continue' : 'Message…',
  );
  let sending = $state(false);

  async function send() {
    const text = (app.view.drafts[key] ?? '').trim();
    if (!text || disabled || sending) return;
    sending = true;
    const r = await app.client.call('session.prompt', { ...onSession(s), content: textContent(text), origin: { kind: 'user', device: app.client.deviceRef }, if_busy: 'queue' });
    sending = false;
    if (!r) return;
    app.view.drafts[key] = '';
    if (r.outcome === 'queued') app.toasts.show('Queued', 'ok');
  }
  const stop = () => app.client.call('session.cancel', onSession(s));

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      if (isBusy && !disabled) void stop();
      else (e.currentTarget as HTMLElement).blur();
    }
  }
</script>

<div class="composer" data-pane="composer">
  <textarea id="composer-{s.id}" rows={app.desk ? 2 : 1} {placeholder} {disabled} aria-label="Message" bind:value={app.view.drafts[key]} onkeydown={onkey}></textarea>
  <div class="acts">
    {#if isBusy}<button class="btn danger" title="Stop (Esc)" {disabled} onclick={stop}><span class="kbd">Esc</span> Stop</button>{/if}
    <button class="btn primary" disabled={disabled || sending || !(app.view.drafts[key] ?? '').trim()} onclick={send}><span class="kbd">↵</span> {isBusy ? 'Queue' : 'Send'}</button>
  </div>
</div>

<style>
  .composer { display: flex; gap: 8px; align-items: flex-end; max-width: 900px; width: 100%; margin: 0 auto; padding: 12px 40px 22px; }
  textarea { flex: 1; border-radius: 12px; background: var(--bg3); border: none; padding: 12px 14px; }
  .acts { display: flex; gap: 6px; }
  @media (max-width: 760px) {
    .composer { padding: 8px 12px calc(env(safe-area-inset-bottom) + 10px); gap: 6px; }
    textarea { font-size: 16px; }
    .btn { padding: 10px 14px; }
  }
</style>
