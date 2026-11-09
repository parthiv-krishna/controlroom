<script lang="ts">
  // A subscription sign-in, driven by pi on the server: pick a method, open the sign-in page and
  // paste the URL the browser ends on (the callback listener is on the server), or enter a device
  // code. Follows the steps with long polls until done, failed, cancelled or timed out.
  import { onDestroy } from 'svelte';
  import { useApp } from '../app/app.svelte';
  import { isFinal } from '../model/models';
  import type { LoginState, ModelProvider } from '../protocol';

  let { provider, ondone }: { provider: ModelProvider; ondone: (signedIn: boolean) => void } = $props();
  const app = useApp();
  let login = $state<LoginState | null>(null);
  let paste = $state('');
  let answer = $state('');
  let alive = true;
  // The running sign-in, outside of state: teardown reads state as it was before the update that
  // destroyed this component, which may be the step before `done`.
  let running: string | null = null;
  onDestroy(() => {
    alive = false;
    if (running) void app.client.request('models.login_cancel', { login_id: running }).catch(() => {});
  });

  // A newer state wins; responses can arrive out of order.
  const take = (s: LoginState | undefined) => {
    if (!s || !alive || (login && s.login_id === login.login_id && s.seq <= login.seq)) return;
    login = s;
    running = isFinal(s.step) ? null : s.login_id;
  };

  async function follow() {
    while (alive && login && !isFinal(login.step)) {
      const { login_id, seq } = login;
      try {
        take(await app.client.request('models.login_wait', { login_id, after: seq }));
      } catch (e) {
        if (alive) app.toasts.show(`Sign-in: ${e instanceof Error ? e.message : String(e)}`, 'error');
        return;
      }
    }
    if (alive && login?.step.kind === 'done') {
      app.toasts.show(`Signed in to ${provider.label}`, 'ok');
      ondone(true);
    }
  }

  async function start() {
    const s = await app.client.call('models.login_start', { provider: provider.id });
    if (!s) return ondone(false);
    take(s);
    void follow();
  }
  void start();

  async function send(input: string) {
    if (!login || !input.trim()) return;
    take(await app.client.call('models.login_input', { login_id: login.login_id, input: input.trim() }));
  }
  async function cancel() {
    if (login && !isFinal(login.step)) take(await app.client.call('models.login_cancel', { login_id: login.login_id }));
    ondone(false);
  }
  function onEnter(e: KeyboardEvent, run: () => void) {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    e.stopPropagation();
    run();
  }
</script>

<div class="signin" role="group" aria-label="Sign in to {provider.label}">
  {#if !login || login.step.kind === 'progress'}
    <div class="row"><span class="spin"></span><span class="muted">{login?.step.kind === 'progress' ? login.step.message : 'Starting'}</span></div>
  {:else if login.step.kind === 'choose'}
    <div class="small muted">{login.step.message}</div>
    <div class="row wrap">
      {#each login.step.options as o (o.id)}<button class="btn sm" onclick={() => send(o.id)}>{o.label}</button>{/each}
    </div>
  {:else if login.step.kind === 'open_url'}
    <a class="btn sm primary" href={login.step.url} target="_blank" rel="noopener noreferrer">Open sign-in page ↗</a>
    <label class="field">
      <span class="small muted">Then paste the URL or code you end on</span>
      <div class="row">
        <input aria-label="Redirect URL or code for {provider.label}" placeholder="http://localhost:…/callback?code=…" autocomplete="off" spellcheck="false" bind:value={paste} onkeydown={(e) => onEnter(e, () => send(paste))} />
        <button class="btn sm" disabled={!paste.trim()} onclick={() => send(paste)}>Submit</button>
      </div>
    </label>
  {:else if login.step.kind === 'device_code'}
    <div class="row wrap"><span class="small muted">Enter</span><code class="code">{login.step.user_code}</code><span class="small muted">at</span><a href={login.step.verification_uri} target="_blank" rel="noopener noreferrer">{login.step.verification_uri}</a></div>
    <div class="row"><span class="spin"></span><span class="small muted">Waiting</span></div>
  {:else if login.step.kind === 'input'}
    <div class="row">
      <input aria-label={login.step.message} placeholder={login.step.placeholder ?? ''} autocomplete="off" bind:value={answer} onkeydown={(e) => onEnter(e, () => send(answer))} />
      <button class="btn sm" disabled={!answer.trim()} onclick={() => send(answer)}>Submit</button>
    </div>
  {:else if login.step.kind === 'done'}
    <div class="ok-t">Signed in{login.step.account ? ` as ${login.step.account}` : ''}</div>
  {:else if login.step.kind === 'failed'}
    <div class="bad-t" role="alert">Sign-in failed: {login.step.message}</div>
  {:else if login.step.kind === 'timed_out'}
    <div class="bad-t" role="alert">Sign-in timed out</div>
  {:else}
    <div class="muted">Sign-in stopped</div>
  {/if}
  <div class="row">
    {#if login && isFinal(login.step)}
      {#if login.step.kind !== 'done'}<button class="btn sm" onclick={() => { login = null; paste = ''; void start(); }}>Retry</button>{/if}
      <button class="btn sm ghost" onclick={() => ondone(login?.step.kind === 'done')}>Close</button>
    {:else}
      <button class="btn sm ghost" aria-label="Cancel sign-in to {provider.label}" onclick={cancel}>Cancel</button>
    {/if}
  </div>
</div>

<style>
  .signin { display: flex; flex-direction: column; gap: 8px; border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; background: var(--bg3); }
  .row { display: flex; gap: 6px; align-items: center; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .primary { align-self: flex-start; text-decoration: none; }
  .code { font-size: 14px; letter-spacing: 0.08em; }
  .ok-t { color: var(--ok); }
</style>
