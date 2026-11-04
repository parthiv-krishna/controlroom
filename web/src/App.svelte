<script lang="ts">
  import { untrack } from 'svelte';
  import type { Transport } from './lib/api/transport';
  import { App, provideApp } from './lib/app/app.svelte';
  import { onCaptureKey, onKey } from './lib/keys/keyboard';
  import { trackPaneFocus } from './lib/keys/navigator';
  import Help from './lib/keys/Help.svelte';
  import Palette from './lib/keys/Palette.svelte';
  import ModalHost from './lib/modals/ModalHost.svelte';
  import { parseDeepLink } from './lib/model/misc';
  import { attentionOf } from './lib/model/status';
  import Settings from './lib/settings/Settings.svelte';
  import Desktop from './lib/shell/Desktop.svelte';
  import Login from './lib/shell/Login.svelte';
  import Offline from './lib/shell/Offline.svelte';
  import Phone from './lib/shell/Phone.svelte';
  import Rail from './lib/shell/Rail.svelte';
  import { Client } from './lib/state/client.svelte';
  import { loadDevice } from './lib/state/device';
  import DialogHost from './lib/ui/DialogHost.svelte';
  import ToastHost from './lib/ui/ToastHost.svelte';

  let { transport }: { transport: Transport } = $props();

  // svelte-ignore state_referenced_locally
  const app = provideApp(new App(new Client(transport, loadDevice())));
  const { client, view, prefs } = app;
  const deepLink = parseDeepLink(location.hash);

  // Persist preferences whenever one changes.
  $effect(() => prefs.save());

  // Theme: light, dark or the system's.
  let systemDark = $state(matchMedia('(prefers-color-scheme: dark)').matches);
  $effect(() => {
    document.documentElement.dataset.theme = prefs.theme === 'system' ? (systemDark ? 'dark' : 'light') : prefs.theme;
  });

  // The tab title counts what needs a decision or a fix (server-owned attention).
  $effect(() => {
    const n = client.feed.sessionList.filter((s) => {
      const a = attentionOf(s);
      return a === 'approval' || a === 'problem';
    }).length;
    document.title = (n ? `(${n}) ` : '') + 'controlroom';
  });

  // First load: open the deep-linked session, else the most urgent one (desktop only).
  let opened = false;
  $effect(() => {
    if (opened || !client.feed.loaded) return;
    opened = true;
    if (deepLink) {
      const linked = client.feed.sessions.get(deepLink.id);
      if (linked) return app.open(linked);
      app.toasts.show('That session is no longer available', 'error');
    }
    const first = app.orderedSessions()[0];
    if (first && app.desk) app.open(first);
  });

  // Hold the open session's feed (detail) while it is shown.
  $effect(() => {
    const cur = view.current;
    if (!cur) return;
    // The feed map is reactive; only the open session should re-run this.
    const feed = untrack(() => client.sessions.acquire(cur.id, client.online));
    return () => untrack(() => client.sessions.release(feed));
  });

  // Opening a session reads it.
  $effect(() => {
    const s = app.current;
    if (s?.unread && client.online && (app.desk || view.phoneDetail)) app.markRead(s);
  });

  // client.focus: whenever the shown session, layout, visibility or connection changes.
  $effect(() => {
    void view.current;
    void view.phoneDetail;
    void view.settings;
    void view.wide;
    app.reportFocus();
  });

  $effect(() => {
    const untrack = trackPaneFocus();
    const scheme = matchMedia('(prefers-color-scheme: dark)');
    const onScheme = () => (systemDark = scheme.matches);
    const onResize = () => (view.wide = window.innerWidth > 760);
    const onHash = () => {
      const link = parseDeepLink(location.hash);
      const s = link && client.feed.sessions.get(link.id);
      if (s && view.current?.id !== s.id) app.open(s);
    };
    const onVisible = () => app.reportFocus();
    const onCapture = (e: KeyboardEvent) => onCaptureKey(app, e);
    scheme.addEventListener('change', onScheme);
    window.addEventListener('resize', onResize);
    window.addEventListener('hashchange', onHash);
    window.addEventListener('focus', onVisible);
    window.addEventListener('blur', onVisible);
    document.addEventListener('visibilitychange', onVisible);
    window.addEventListener('keydown', onCapture, true);
    return () => {
      untrack();
      scheme.removeEventListener('change', onScheme);
      window.removeEventListener('resize', onResize);
      window.removeEventListener('hashchange', onHash);
      window.removeEventListener('focus', onVisible);
      window.removeEventListener('blur', onVisible);
      document.removeEventListener('visibilitychange', onVisible);
      window.removeEventListener('keydown', onCapture, true);
    };
  });

  const settingsPage = $derived(!!view.settings && prefs.settingsPage && app.desk);
</script>

<svelte:window onkeydown={(e) => onKey(app, e)} />

{#if client.auth.status === 'login'}
  <Login loginUrl={client.auth.login_url} />
{:else if !client.everConnected}
  <Offline link={deepLink} />
{:else}
  {#if client.conn !== 'open'}
    <div class="conn" role="status"><span class="spin"></span> Can't reach server · retrying…</div>
  {/if}
  {#if settingsPage}
    <div class="settings-page"><Rail /><Settings page /></div>
  {:else}
    {#if app.desk}<Desktop />{:else}<Phone />{/if}
    {#if view.settings}<Settings page={false} />{/if}
  {/if}
  <ModalHost />
  <Palette />
  <Help />
{/if}
<DialogHost />
<ToastHost />

<style>
  .conn {
    position: fixed; top: 8px; left: 50%; transform: translateX(-50%); z-index: 350; background: var(--bg2);
    border: 1px solid var(--line); border-radius: 999px; padding: 6px 14px; font-size: 12.5px; box-shadow: var(--shadow);
    display: flex; gap: 8px; align-items: center;
  }
  .settings-page { display: grid; height: 100%; grid-template-columns: var(--rail-w) minmax(0, 1fr); grid-template-areas: 'rail main'; }
</style>
