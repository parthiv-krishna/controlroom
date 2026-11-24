<script lang="ts">
  // Settings › Models: model providers on the server. Presets take an API key or a subscription
  // sign-in, custom OpenAI-compatible gateways take a base URL, models and a key, and the server's
  // own config shows read-only (it wins on a clash). Keys are sent once and never shown again.
  // Defaults pick the model and thinking level of new sessions. A preset or gateway can have
  // several named accounts (e.g. personal and work), each with its own key or sign-in and usage,
  // listed under it. Changes apply to new sessions and restarts.
  // With a profile picked, its policy comes first: "Accounts in <profile>: Any / Only the checked
  // ones" (a checkbox per account; the others dim), and the defaults are the profile's own, on top
  // of the server-wide ones. A private profile lists its accounts (never Any).
  import { useApp } from '../app/app.svelte';
  import { hasAccounts, providerName, usageLine } from '../model/accounts';
  import { admits } from '../model/profiles';
  import { AUTH_HEADERS, THINKING_LEVELS, defaultProviders, isToken, parseModels, providerDetail, providerHistory, providerStatus } from '../model/models';
  import type { AuthHeader, ModelDefaults, ModelPolicy, ModelProvider, ModelsState } from '../protocol';
  import Dropdown from '../ui/Dropdown.svelte';
  import SignIn from './SignIn.svelte';

  const app = useApp();
  const online = $derived(app.client.online);
  let models = $state<ModelsState | null>(null);
  const reload = () => app.client.call('models.list', {}).then((r) => r && (models = r));
  $effect(() => {
    if (online) void reload();
  });

  $effect(() => {
    if (models) app.accounts.update(models.providers);
  });

  const all = $derived(models?.providers ?? []);
  // A gateway's accounts are listed with it.
  const gatewayIds = $derived(new Set(all.filter((p) => p.gateway).map((p) => p.id)));
  const isGateway = (p: ModelProvider) => !!p.gateway || (!!p.account_of && gatewayIds.has(p.account_of));
  const providers = $derived(all.filter((p) => !isGateway(p)));
  const gateways = $derived(all.filter(isGateway));
  const nameOf = (p: ModelProvider) => providerName(p, all);
  const deviceLabel = (id: string, fallback: string) => app.client.feed.devices.get(id)?.label ?? fallback;

  // One open form at a time: a key, a sign-in, an account's name or the gateway form.
  let keyFor = $state<string | null>(null);
  let key = $state('');
  /** Adding an account to this provider, or renaming this account. */
  let accountForm = $state<{ id: string; rename: boolean } | null>(null);
  let accountName = $state('');
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
    accountForm = null;
    accountName = '';
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
    const what = p.account_of
      ? `the account ${p.account} of ${p.label}`
      : p.gateway
        ? `the gateway ${p.label}`
        : `the ${nameOf(p)} ${isToken(p.key) ? 'subscription token' : 'key'}`;
    const body = p.account_of ? 'Its key or sign-in is deleted. New sessions and restarts stop using it.' : 'New sessions and restarts stop using it.';
    if (!(await app.dialogs.confirm({ title: `Remove ${what}?`, body, ok: 'Remove', danger: true }))) return;
    await run(p.id, () => app.client.call('models.remove', { provider: p.id }));
  }
  function editAccount(p: ModelProvider, rename: boolean) {
    closeForms();
    accountForm = { id: p.id, rename };
    accountName = rename ? (p.account ?? '') : '';
  }
  async function saveAccount() {
    if (!accountForm) return;
    const { id, rename } = accountForm;
    const name = accountName.trim();
    if (!name) return;
    const r = rename
      ? await run(id, () => app.client.call('models.rename_account', { provider: id, name }))
      : await run(id, () => app.client.call('models.add_account', { provider: id, name }));
    if (r) closeForms();
  }
  async function signOut(p: ModelProvider) {
    if (!(await app.dialogs.confirm({ title: `Sign out of ${nameOf(p)}?`, body: 'New sessions and restarts stop using it.', ok: 'Sign out', danger: true }))) return;
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

  // ---- the picked profile's accounts and defaults
  const profile = $derived(app.settingsProfile);
  /** Accounts that can run something: a key, a sign-in, a gateway or the server's config. */
  const usable = (p: ModelProvider) => !!p.key || !!p.signed_in || !!p.gateway || p.source === 'server';
  const listed = (p: ModelProvider) => !profile || admits(profile.models, p.id);
  async function setPolicy(models: ModelPolicy) {
    if (!profile) return;
    const provider = profile.model_defaults.provider;
    // A default outside the list would be refused: it goes with the account.
    const model_defaults = provider && !admits(models, provider) ? { provider: null, model: null, thinking: profile.model_defaults.thinking } : undefined;
    await app.client.call('profiles.update', { profile_id: profile.id, models, model_defaults });
  }
  /** Only the checked ones starts with what is usable now, so nothing changes until unchecked. */
  function setKind(kind: string) {
    if (kind === 'any') void setPolicy({ kind: 'any' });
    else if (profile?.models.kind !== 'only') void setPolicy({ kind: 'only', accounts: all.filter(usable).map((p) => p.id) });
  }
  function setListed(p: ModelProvider, on: boolean) {
    if (profile?.models.kind !== 'only') return;
    const accounts = profile.models.accounts.filter((id) => id !== p.id);
    void setPolicy({ kind: 'only', accounts: on ? [...accounts, p.id] : accounts });
  }

  /** The defaults shown: the profile's (on top of the server-wide ones), else the server-wide ones. */
  const shown = $derived(profile ? profile.model_defaults : models?.defaults);
  async function setDefaults(change: Partial<ModelDefaults>) {
    if (!models || !shown) return;
    const defaults = { ...shown, ...change };
    if (profile) await run('defaults', () => app.client.call('profiles.update', { profile_id: profile.id, model_defaults: defaults }).then(() => models ?? undefined));
    else await run('defaults', () => app.client.call('models.set_defaults', { defaults }));
  }
  /** What an unset default falls back to: the server-wide default, then the server's config. */
  const fallback = $derived.by(() => {
    if (!models) return { provider: null, thinking: null };
    const base = profile && models.defaults.provider ? models.defaults : models.server_defaults;
    const thinking = (profile && models.defaults.thinking) || models.server_defaults.thinking;
    return { provider: base.provider ? `${base.provider}${base.model ? `/${base.model}` : ''}` : null, thinking };
  });
  const providerOptions = $derived(models ? defaultProviders(models.models, models.providers).filter((o) => !profile || admits(profile.models, o.value)).map((o) => ({ ...o, label: nameFor(o.value, o.label) })) : []);
  const nameFor = (id: string, label: string) => {
    const p = all.find((q) => q.id === id);
    return p ? nameOf(p) : label;
  };
  const modelOptions = $derived(models?.models.filter((m) => m.provider === shown?.provider).map((m) => ({ value: m.id, label: m.name })) ?? []);
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
  {@const name = nameOf(p)}
  {@const multi = hasAccounts(p, all)}
  {@const usage = usageLine(p.usage)}
  <div class="card" class:account={!!p.account_of} class:dim={!listed(p)} data-provider={p.id} title={providerHistory(p, deviceLabel) || undefined}>
    <div class="h">
      <b>{p.label}</b>
      {#if multi}<span class="acct" data-account>{p.account}</span>{/if}
      <span class="st {status.tone}" data-status>{status.text}</span>
      {#if profile?.models.kind === 'only'}
        <label class="in small"><input type="checkbox" checked={listed(p)} disabled={!online} aria-label="Use {name} in {profile.name}" onchange={(e) => setListed(p, e.currentTarget.checked)} /> In {profile.name}</label>
      {/if}
      <span class="sp"></span>
      <div class="acts">
        {#if p.takes_key && !p.gateway}
          <button class="btn xs" disabled={!online} aria-label={p.key ? `Replace ${stored} for ${name}` : `Add ${accepts} for ${name}`} onclick={() => { closeForms(); keyFor = p.id; }}>{p.key ? `Replace ${isToken(p.key) ? 'token' : 'key'}` : 'Add key'}</button>
        {/if}
        {#if p.gateway}<button class="btn xs" disabled={!online} aria-label="Edit gateway {p.label}" onclick={() => editGateway(p)}>Edit</button>{/if}
        {#if p.can_sign_in && !p.signed_in}
          <button class="btn xs" disabled={!online || !!p.key} title={p.key ? `Remove the ${isToken(p.key) ? 'token' : 'key'} first` : undefined} aria-label="Sign in to {name}" onclick={() => { closeForms(); signingIn = p; }}>Sign in</button>
        {/if}
        {#if p.signed_in}<button class="btn xs ghost" disabled={!online || busy === p.id} aria-label="Sign out of {name}" onclick={() => signOut(p)}>Sign out</button>{/if}
        {#if p.key || p.signed_in || readOnly}
          <button class="btn xs ghost" disabled={!online || busy === p.id} aria-label="Test {name}" onclick={() => test(p)}>{#if busy === p.id}<span class="spin"></span>{/if}Test</button>
        {/if}
        {#if p.can_add_account}<button class="btn xs ghost" disabled={!online} aria-label="Add an account to {p.label}" onclick={() => editAccount(p, false)}>+ Account</button>{/if}
        {#if multi}<button class="btn xs ghost" disabled={!online} aria-label="Rename account {p.account} of {p.label}" onclick={() => editAccount(p, true)}>Rename</button>{/if}
        {#if p.account_of}
          <button class="btn xs ghost bad-t" disabled={!online || busy === p.id} aria-label="Remove account {p.account} of {p.label}" onclick={() => remove(p)}>Remove</button>
        {:else if (p.key || p.gateway) && !readOnly}
          <button class="btn xs ghost bad-t" disabled={!online || busy === p.id} aria-label={p.gateway ? `Remove gateway ${p.label}` : `Remove ${stored} for ${name}`} onclick={() => remove(p)}>Remove</button>
        {/if}
      </div>
    </div>
    {#if providerDetail(p)}<div class="small muted detail">{providerDetail(p)}</div>{/if}
    {#if usage}<div class="small muted detail" data-usage title="Tokens and cost of this server's sessions">{usage}</div>{/if}
    {#if p.clash}<div class="small warn-t">{p.clash}</div>{/if}
    {#if keyFor === p.id}
      <div class="form row">
        <!-- svelte-ignore a11y_autofocus -->
        <input type="password" autocomplete="off" spellcheck="false" autofocus aria-label="{accepts} for {name}" placeholder={p.token_prefix ? `API key or ${p.token_prefix}… token` : 'API key'} bind:value={key} onkeydown={(e) => onEnter(e, () => saveKey(p))} />
        <button class="btn sm primary" disabled={!online || !key.trim()} onclick={() => saveKey(p)}>Save</button>
        <button class="btn sm ghost" onclick={closeForms}>Cancel</button>
      </div>
    {/if}
    {#if accountForm?.id === p.id}
      <div class="form row">
        <!-- svelte-ignore a11y_autofocus -->
        <input autocomplete="off" spellcheck="false" autofocus aria-label={accountForm.rename ? `New name of account ${p.account} of ${p.label}` : `Name of the new ${p.label} account`} placeholder="work" bind:value={accountName} onkeydown={(e) => onEnter(e, saveAccount)} />
        <button class="btn sm primary" disabled={!online || !accountName.trim()} onclick={saveAccount}>{accountForm.rename ? 'Rename' : 'Add account'}</button>
        <button class="btn sm ghost" onclick={closeForms}>Cancel</button>
      </div>
    {/if}
    {#if signingIn?.id === p.id}<SignIn provider={{ ...p, label: name }} ondone={signedIn} />{/if}
  </div>
{/snippet}

{#if models}
  <div class="small muted">For new sessions and restarts. Keys stay on the server. Add accounts to keep personal and work use apart.</div>
  {#if profile}
    {@const pf = profile}
    <div class="policy" data-policy>
      <span class="small muted">Accounts in {pf.name}</span>
      <Dropdown small label="Accounts in {pf.name}" value={pf.models.kind === 'any' ? 'any' : 'only'} disabled={!online}
        options={[{ value: 'any', label: 'Any', disabled: pf.private, hint: pf.private ? 'A private profile lists its accounts' : 'Every account, new ones too' }, { value: 'only', label: 'Only the checked ones' }]}
        onchange={setKind} />
    </div>
  {/if}
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

  <div class="sec-h">{profile ? `Defaults in ${profile.name}` : 'Defaults'}</div>
  {#if shown}
    {@const d = shown}
    <div class="defaults" data-defaults>
      <div class="field"><span>Provider</span>
        <Dropdown small label="Default provider" value={d.provider ?? ''} disabled={!online}
          options={[{ value: '', label: fallback.provider ? `Server (${fallback.provider})` : 'pi default' }, ...providerOptions]}
          onchange={(v) => setDefaults({ provider: v || null, model: null })} />
      </div>
      <div class="field"><span>Model</span>
        <Dropdown small label="Default model" value={d.model ?? ''} disabled={!online || !d.provider}
          options={[{ value: '', label: 'Provider default' }, ...modelOptions]}
          onchange={(v) => setDefaults({ model: v || null })} />
      </div>
      <div class="field"><span>Thinking</span>
        <Dropdown small label="Default thinking level" value={d.thinking ?? ''} disabled={!online}
          options={[{ value: '', label: fallback.thinking ? `Server (${fallback.thinking})` : 'pi default' }, ...THINKING_LEVELS.map((l) => ({ value: l, label: l }))]}
          onchange={(v) => setDefaults({ thinking: v || null })} />
      </div>
    </div>
  {/if}
  {#if models.models_error}<div class="small warn-t">Models unavailable: {models.models_error}</div>{/if}
{:else}
  <div class="muted small">{online ? 'Loading…' : 'Not connected'}</div>
{/if}

<style>
  .card { border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; margin-bottom: 6px; display: flex; flex-direction: column; gap: 6px; }
  .card.account { margin-left: 18px; }
  .card.dim > :not(.h), .card.dim .h > :not(.in) { opacity: 0.55; }
  .in { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); white-space: nowrap; }
  .policy { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .acct { font-size: 11.5px; padding: 0 6px; border-radius: 4px; background: var(--bg3); color: var(--fg); white-space: nowrap; }
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
    .card.account { margin-left: 10px; }
  }
</style>
