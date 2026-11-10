<script lang="ts">
  // Settings › Models: model providers on the server. Presets take an API key or a subscription
  // sign-in, custom OpenAI-compatible gateways take a base URL, models and a key, and the server's
  // own config shows read-only (it wins on a clash). Keys are sent once and never shown again.
  // Defaults pick the model and thinking level of new sessions. Changes apply to new sessions and
  // restarts.
  import { useApp } from '../app/app.svelte';
  import { AUTH_HEADERS, THINKING_LEVELS, defaultProviders, isToken, parseModels, providerDetail, providerStatus } from '../model/models';
  import type { AuthHeader, ModelDefaults, ModelProvider, ModelsState } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';
  import SignIn from './SignIn.svelte';

  const app = useApp();
  const online = $derived(app.client.online);
  let models = $state<ModelsState | null>(null);
  const reload = () => app.client.call('models.list', {}).then((r) => r && (models = r));
  $effect(() => {
    if (online) void reload();
  });

  const providers = $derived(models?.providers.filter((p) => !p.gateway) ?? []);
  const gateways = $derived(models?.providers.filter((p) => p.gateway) ?? []);
  const deviceLabel = (id: string, fallback: string) => app.client.feed.devices.get(id)?.label ?? fallback;

  // One open form at a time: a key, a sign-in or the gateway form.
  let keyFor = $state<string | null>(null);
  let key = $state('');
  let signingIn = $state<ModelProvider | null>(null);
  let busy = $state<string | null>(null);
  interface GatewayForm {
    existing: boolean;
    name: string;
    baseUrl: string;
    models: string;
    auth: AuthHeader;
    key: string;
  }
  let gw = $state<GatewayForm | null>(null);

  function closeForms() {
    keyFor = null;
    key = '';
    signingIn = null;
    gw = null;
  }
  async function run(id: string, call: () => Promise<ModelsState | undefined>) {
    busy = id;
    try {
      const r = await call();
      if (r) models = r;
      return r;
    } finally {
      busy = null;
    }
  }
  async function saveKey(p: ModelProvider) {
    const value = key.trim();
    key = '';
    if (!value) return;
    if (await run(p.id, () => app.client.call('models.set_key', { provider: p.id, key: value }))) keyFor = null;
  }
  async function remove(p: ModelProvider) {
    const what = p.gateway ? `the gateway ${p.label}` : `the ${p.label} ${isToken(p.key) ? 'subscription token' : 'key'}`;
    if (!(await app.dialogs.confirm({ title: `Remove ${what}?`, body: 'New sessions and restarts stop using it.', ok: 'Remove', danger: true }))) return;
    await run(p.id, () => app.client.call('models.remove', { provider: p.id }));
  }
  async function signOut(p: ModelProvider) {
    if (!(await app.dialogs.confirm({ title: `Sign out of ${p.label}?`, body: 'New sessions and restarts stop using it.', ok: 'Sign out', danger: true }))) return;
    await run(p.id, () => app.client.call('models.sign_out', { provider: p.id }));
  }
  const test = (p: ModelProvider) => run(p.id, () => app.client.call('models.test', { provider: p.id }));
  function signedIn(ok: boolean) {
    signingIn = null;
    if (ok) void reload();
  }

  function editGateway(p?: ModelProvider) {
    closeForms();
    gw = p?.gateway
      ? { existing: true, name: p.id, baseUrl: p.gateway.base_url, models: p.gateway.models.join(', '), auth: p.gateway.auth_header, key: '' }
      : { existing: false, name: '', baseUrl: 'https://', models: '', auth: 'bearer', key: '' };
  }
  async function saveGateway() {
    if (!gw) return;
    const form = gw;
    const params = {
      provider: form.name.trim(),
      gateway: { base_url: form.baseUrl.trim(), models: parseModels(form.models), auth_header: form.auth },
      key: form.key.trim() || null,
    };
    form.key = '';
    if (await run(params.provider, () => app.client.call('models.set_gateway', params))) gw = null;
  }

  async function setDefaults(change: Partial<ModelDefaults>) {
    if (!models) return;
    const defaults = { ...models.defaults, ...change };
    await run('defaults', () => app.client.call('models.set_defaults', { defaults }));
  }
  const providerOptions = $derived(models ? defaultProviders(models.models, models.providers) : []);
  const modelOptions = $derived(models?.models.filter((m) => m.provider === models?.defaults.provider).map((m) => ({ value: m.id, label: m.name })) ?? []);
  const serverDefault = $derived(
    models?.server_defaults.provider ? `${models.server_defaults.provider}${models.server_defaults.model ? `/${models.server_defaults.model}` : ''}` : null,
  );
  function onEnter(e: KeyboardEvent, run: () => void) {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    e.stopPropagation();
    run();
  }
</script>

{#snippet row(p: ModelProvider)}
  {@const status = providerStatus(p)}
  {@const readOnly = p.source === 'server'}
  {@const stored = isToken(p.key) ? 'subscription token' : 'API key'}
  {@const accepts = p.token_prefix ? 'API key or subscription token' : 'API key'}
  <div class="card" data-provider={p.id}>
    <div class="h">
      <b>{p.label}</b>
      <span class="st {status.tone}" data-status>{status.text}</span>
      <span class="sp"></span>
      <div class="acts">
        {#if p.takes_key && !p.gateway}
          <button class="btn xs" disabled={!online} aria-label={p.key ? `Replace ${stored} for ${p.label}` : `Add ${accepts} for ${p.label}`} onclick={() => { closeForms(); keyFor = p.id; }}>{p.key ? `Replace ${isToken(p.key) ? 'token' : 'key'}` : 'Add key'}</button>
        {/if}
        {#if p.gateway}<button class="btn xs" disabled={!online} aria-label="Edit gateway {p.label}" onclick={() => editGateway(p)}>Edit</button>{/if}
        {#if p.can_sign_in && !p.signed_in}
          <button class="btn xs" disabled={!online || !!p.key} title={p.key ? `Remove the ${isToken(p.key) ? 'token' : 'key'} first` : undefined} aria-label="Sign in to {p.label}" onclick={() => { closeForms(); signingIn = p; }}>Sign in</button>
        {/if}
        {#if p.signed_in}<button class="btn xs ghost" disabled={!online || busy === p.id} aria-label="Sign out of {p.label}" onclick={() => signOut(p)}>Sign out</button>{/if}
        {#if p.key || p.signed_in || readOnly}
          <button class="btn xs ghost" disabled={!online || busy === p.id} aria-label="Test {p.label}" onclick={() => test(p)}>{#if busy === p.id}<span class="spin"></span>{/if}Test</button>
        {/if}
        {#if (p.key || p.gateway) && !readOnly}<button class="btn xs ghost bad-t" disabled={!online || busy === p.id} aria-label={p.gateway ? `Remove gateway ${p.label}` : `Remove ${stored} for ${p.label}`} onclick={() => remove(p)}>Remove</button>{/if}
      </div>
    </div>
    {#if providerDetail(p, deviceLabel)}<div class="small muted detail">{providerDetail(p, deviceLabel)}</div>{/if}
    {#if p.clash}<div class="small warn-t">{p.clash}</div>{/if}
    {#if keyFor === p.id}
      <div class="form row">
        <!-- svelte-ignore a11y_autofocus -->
        <input type="password" autocomplete="off" spellcheck="false" autofocus aria-label="{accepts} for {p.label}" placeholder={p.token_prefix ? `API key or ${p.token_prefix}… token` : 'API key'} bind:value={key} onkeydown={(e) => onEnter(e, () => saveKey(p))} />
        <button class="btn sm primary" disabled={!online || !key.trim()} onclick={() => saveKey(p)}>Save</button>
        <button class="btn sm ghost" onclick={closeForms}>Cancel</button>
      </div>
    {/if}
    {#if signingIn?.id === p.id}<SignIn provider={p} ondone={signedIn} />{/if}
  </div>
{/snippet}

{#if models}
  <div class="small muted">For new sessions and restarts. Keys stay on the server.</div>
  <div class="sec-h">Providers</div>
  {#each providers as p (p.id)}{@render row(p)}{/each}

  <div class="sec-h">Gateways <span class="sp"></span><button class="btn xs" disabled={!online} onclick={() => editGateway()}>+ Gateway</button></div>
  {#each gateways as p (p.id)}{@render row(p)}{:else}{#if !gw}<div class="small muted">None</div>{/if}{/each}
  {#if gw}
    <div class="card form" role="group" aria-label={gw.existing ? `Edit gateway ${gw.name}` : 'New gateway'}>
      <div class="grid">
        <label class="field"><span>Name</span><input aria-label="Gateway name" placeholder="gateway" disabled={gw.existing} bind:value={gw.name} /></label>
        <label class="field"><span>Base URL</span><input aria-label="Gateway base URL" placeholder="https://llm.example.com/v1" spellcheck="false" bind:value={gw.baseUrl} /></label>
        <label class="field"><span>Models</span><input aria-label="Gateway models" placeholder="model-a, model-b" spellcheck="false" bind:value={gw.models} /></label>
        <div class="field"><span>Key header</span><Dropdown small label="Gateway key header" value={gw.auth} options={AUTH_HEADERS} onchange={(v) => gw && (gw.auth = v as AuthHeader)} /></div>
        <label class="field"><span>API key</span><input type="password" autocomplete="off" aria-label="Gateway API key" placeholder={gw.existing ? 'unchanged' : 'API key'} bind:value={gw.key} /></label>
      </div>
      <div class="row">
        <button class="btn sm primary" disabled={!online || !gw.name.trim() || !parseModels(gw.models).length || (!gw.existing && !gw.key.trim())} onclick={saveGateway}>Save gateway</button>
        <button class="btn sm ghost" onclick={() => (gw = null)}>Cancel</button>
      </div>
    </div>
  {/if}

  <div class="sec-h">Defaults</div>
  <div class="defaults" data-defaults>
    <div class="field"><span>Provider</span>
      <Dropdown small label="Default provider" value={models.defaults.provider ?? ''} disabled={!online}
        options={[{ value: '', label: serverDefault ? `Server (${serverDefault})` : 'pi default' }, ...providerOptions]}
        onchange={(v) => setDefaults({ provider: v || null, model: null })} />
    </div>
    <div class="field"><span>Model</span>
      <Dropdown small label="Default model" value={models.defaults.model ?? ''} disabled={!online || !models.defaults.provider}
        options={[{ value: '', label: 'Provider default' }, ...modelOptions]}
        onchange={(v) => setDefaults({ model: v || null })} />
    </div>
    <div class="field"><span>Thinking</span>
      <Dropdown small label="Default thinking level" value={models.defaults.thinking ?? ''} disabled={!online}
        options={[{ value: '', label: models.server_defaults.thinking ? `Server (${models.server_defaults.thinking})` : 'pi default' }, ...THINKING_LEVELS.map((l) => ({ value: l, label: l }))]}
        onchange={(v) => setDefaults({ thinking: v || null })} />
    </div>
  </div>
  {#if models.models_error}<div class="small warn-t">Models unavailable: {models.models_error}</div>{/if}
{:else}
  <div class="muted small">{online ? 'Loading…' : 'Not connected'}</div>
{/if}

<style>
  .card { border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; margin-bottom: 6px; display: flex; flex-direction: column; gap: 6px; }
  .h { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .acts { display: flex; gap: 4px; flex-wrap: wrap; }
  .st { font-size: 11.5px; padding: 0 8px; border-radius: 999px; border: 1px solid currentColor; white-space: nowrap; }
  .st.ok { color: var(--ok); }
  .st.bad { color: var(--bad); }
  .st.warn { color: var(--warn); }
  .st.muted { color: var(--muted); border-color: var(--line); }
  .detail { overflow-wrap: anywhere; }
  .row { display: flex; gap: 6px; align-items: center; }
  .form input { min-width: 0; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 8px 10px; }
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--muted); min-width: 0; }
  .defaults { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; }
  @media (max-width: 760px) {
    .grid, .defaults { grid-template-columns: 1fr; }
    .h .sp { display: none; }
    .acts { width: 100%; }
  }
</style>
