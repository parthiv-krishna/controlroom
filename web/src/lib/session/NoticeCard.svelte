<script lang="ts">
  // The agent's open warning or error notice (`notify_user`), pinned until the user dismisses it:
  // its level icon, its Markdown text and Dismiss. The agent isn't told. A subagent's notice is
  // dismissed on the subagent. Used in the session's tray and the notifications panel (compact).
  import { newOpId, useApp } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import type { FeedSession, OpenNotice } from '../protocol';
  import Icon from '../ui/Icon.svelte';
  import Markdown from '../ui/Markdown.svelte';

  let { notice, s, compact = false }: { notice: OpenNotice; s: FeedSession; compact?: boolean } = $props();
  const app = useApp();
  const targetId = $derived(notice.origin_child ?? s.id);
  const child = $derived(notice.origin_child ? app.client.feed.sessions.get(notice.origin_child) : undefined);
  const level = $derived(notice.level === 'error' ? 'Error' : 'Warning');
  let busy = $state(false);

  async function dismiss() {
    busy = true;
    await app.client.call('notice.dismiss', { op_id: newOpId(), id: targetId, item_id: notice.item_id });
    busy = false;
  }
</script>

<div class="nt nt-{notice.level}" class:compact data-notice={notice.item_id}>
  <span class="ic"><Icon name={notice.level === 'error' ? 'cross' : 'warn'} size={13} label={level} /></span>
  <div class="body">
    {#if notice.origin_child}
      <div class="from">from subagent <button class="linkish" onclick={() => app.open({ id: targetId })}>{child ? displayName(child) : 'subagent'}</button></div>
    {/if}
    <div class="text"><Markdown text={notice.text} /></div>
  </div>
  <button class="btn ghost" class:sm={compact} aria-label="Dismiss {level.toLowerCase()} notice" disabled={!app.client.online || busy} onclick={dismiss}>Dismiss</button>
</div>

<style>
  .nt {
    display: flex; align-items: flex-start; gap: 9px; padding: 9px 12px; border-radius: 10px;
    border: 1px solid color-mix(in srgb, var(--warn) 55%, var(--line)); background: var(--perm-bg);
  }
  .nt-error { border-color: color-mix(in srgb, var(--bad) 55%, var(--line)); }
  .compact { padding: 7px 10px; font-size: 12.5px; }
  .ic { color: var(--warn); padding-top: 3px; }
  .nt-error .ic { color: var(--bad); }
  .body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; font-size: 13.5px; overflow-wrap: anywhere; }
  .compact .body { font-size: 13px; }
  .compact .text { display: -webkit-box; -webkit-line-clamp: 4; line-clamp: 4; -webkit-box-orient: vertical; overflow: hidden; }
  .from { font-size: 12px; color: var(--muted); }
  .btn { flex: none; align-self: center; }
  @media (max-width: 760px) { .btn:not(.sm) { padding: 9px 13px; } }
</style>
