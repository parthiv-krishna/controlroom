<script lang="ts">
  // Sandbox defaults (spec §11.6) per machine and project: level, standing path grants, the "never"
  // list (paths denied without asking), sockets the sandbox may connect to, allowed domains and
  // mediated commands. A session gets the strictest level of its machine and projects and the union
  // of the rest; edits apply to new sessions and restarts. A granted directory includes everything
  // under it (Landlock grants are recursive), so there is no separate toggle for that. Grants that
  // could never take effect are refused with the reason (checked here, and by the server).
  // With a profile picked, its own card (its level, rules for all its sessions) comes first, then
  // its machines and projects; a shared machine can't be set to off.
  import { useApp } from '../app/app.svelte';
  import { describeError } from '../api/errors';
  import { KNOWN_SOCKETS, LEVELS, PROC_GRANT, checkDefaultsPath, grantLimit, levelLabel } from '../model/sandbox';
  import type { MediationAction, NewGrant, PathAccess, DefaultsChange, DefaultsScope, SandboxDefaults } from '../protocol';
  import ActionMenu, { type MenuAction } from '../ui/ActionMenu.svelte';
  import Dropdown from '../ui/Dropdown.svelte';

  const app = useApp();
  const online = $derived(app.client.online);
  let defaults = $state<SandboxDefaults[]>([]);
  $effect(() => {
    if (online) void app.client.call('sandbox.defaults', {}).then((r) => (defaults = r?.defaults ?? []));
  });

  interface Row {
    key: string;
    name: string;
    /** The name in accessible labels (a profile may be named like a machine). */
    label: string;
    /** Shared with another profile: never off. */
    shared: boolean;
    color: string | null;
    scope: DefaultsScope;
    defaults: SandboxDefaults | null;
  }
  const scopeKey = (s: DefaultsScope) =>
    s.kind === 'machine' ? `m:${s.machine_id}` : s.kind === 'project' ? `p:${s.project_id}` : s.kind === 'profile' ? `f:${s.profile_id}` : '';
  const pf = $derived(app.settingsProfile);
  const rows = $derived.by(() => {
    const byKey = new Map(defaults.map((p) => [scopeKey(p.scope), p]));
    const scoped = app.ctxOf(pf?.id);
    const profile: Row[] = pf ? [{ key: `f:${pf.id}`, name: pf.name, label: `profile ${pf.name}`, shared: false, color: pf.color, scope: { kind: 'profile', profile_id: pf.id }, defaults: byKey.get(`f:${pf.id}`) ?? null }] : [];
    const machines: Row[] = scoped.machineList.map((m) => ({ key: `m:${m.id}`, name: m.name, label: m.name, shared: m.profile_ids.length > 1, color: null, scope: { kind: 'machine', machine_id: m.id }, defaults: byKey.get(`m:${m.id}`) ?? null }));
    const projects: Row[] = scoped.projects.map((p) => ({ key: `p:${p.id}`, name: p.name, label: p.name, shared: false, color: p.color, scope: { kind: 'project', project_id: p.id }, defaults: byKey.get(`p:${p.id}`) ?? null }));
    const groups: [string, Row[]][] = [['Profile', profile], ['Machines', machines], ['Projects', projects]];
    return groups.filter(([, rows]) => rows.length);
  });
  /** A machine's unset level falls back to its profile's ("Garden club default (strict)"). */
  const unset = (row: Row): string => {
    if (row.scope.kind === 'project') return 'No minimum';
    if (row.scope.kind === 'machine' && pf) {
      const level = defaults.find((d) => d.scope.kind === 'profile' && d.scope.profile_id === pf.id)?.level ?? 'strict';
      return `${pf.name} default (${levelLabel(level).toLowerCase()})`;
    }
    return 'Default (strict)';
  };
  const levelOptions = (row: Row) => [
    { value: 'none', label: unset(row) },
    ...LEVELS.map((l) => (l.value === 'off' && row.shared ? { value: l.value, label: l.label, disabled: true, hint: `${row.name} is shared; it can’t run unsandboxed` } : { value: l.value, label: l.label, hint: l.hint })),
  ];

  type Kind = 'grant' | 'socket' | 'never' | 'domain' | 'command';
  /** The rule input open on each card (only rules that exist show otherwise). */
  let adding = $state<Record<string, Kind>>({});
  const SOCKET_NOTE = 'Sandboxed programs can connect only to the Unix sockets listed here. nono can’t ask, so any other connection fails at once ("Permission denied" or "Operation not permitted").';
  /** The "+ rule" menu: a rule input to open, then the one-click grants not yet there. */
  function ruleActions(row: Row, paths: { value: string }[], sockets: { value: string }[]): MenuAction[] {
    const open = (k: Kind) => () => (adding[row.key] = k);
    return [
      { label: 'Path grant…', run: open('grant') },
      { label: 'Socket…', hint: SOCKET_NOTE, run: open('socket') },
      { label: 'Never path…', hint: 'Denied without asking', run: open('never') },
      { label: 'Domain…', run: open('domain') },
      { label: 'Command…', hint: 'Ask or deny before it runs', run: open('command') },
      ...(paths.some((g) => g.value === PROC_GRANT.path) ? [] : [{ label: PROC_GRANT.label, hint: PROC_GRANT.hint, aria: `Grant ${PROC_GRANT.label} on ${row.label}`, disabled: !online, run: () => void addGrant(row, PROC_GRANT.path, 'read') }]),
      ...KNOWN_SOCKETS.filter((k) => !sockets.some((g) => g.value === k.path)).map((k) => ({ label: k.label, hint: `${k.path}: ${k.hint}`, aria: `Allow ${k.label} (${k.path}) on ${row.label}`, disabled: !online, run: () => void addSocket(row, k.path) })),
    ];
  }
  let domain = $state<Record<string, string>>({});
  let command = $state<Record<string, string>>({});
  let action = $state<Record<string, MediationAction>>({});
  let grantPath = $state<Record<string, string>>({});
  let grantAccess = $state<Record<string, PathAccess>>({});
  let neverPath = $state<Record<string, string>>({});
  let socketPath = $state<Record<string, string>>({});
  /** Why the last path typed for a row (`<key>:grant`, `<key>:never` or `<key>:socket`) was refused. */
  let pathError = $state<Record<string, string>>({});

  async function change(scope: DefaultsScope, c: DefaultsChange) {
    const r = await app.client.call('sandbox.set_defaults', { scope, change: c });
    if (r) defaults = r.defaults;
  }
  async function setLevel(row: Row, v: string) {
    if (v === 'off' && !(await app.dialogs.confirm({ title: `Turn the sandbox off for ${row.label}?`, body: 'New sessions there run unconfined; running ones keep their sandbox.', ok: 'Turn off', danger: true }))) return;
    await change(row.scope, v === 'none' ? { kind: 'clear_level' } : { kind: 'set_level', level: v as SandboxDefaults['level'] & string });
  }
  function onEnter(e: KeyboardEvent, run: () => void) {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    e.stopPropagation();
    run();
  }
  function addDomain(row: Row) {
    const v = (domain[row.key] ?? '').trim();
    if (!v) return;
    domain[row.key] = '';
    void change(row.scope, { kind: 'add_domain', domain: v });
  }
  /** A typed path, normalized; `null` (with the reason shown) when it is refused. */
  function typedPath(row: Row, which: 'grant' | 'never' | 'socket', value: string): string | null {
    const key = `${row.key}:${which}`;
    const checked = checkDefaultsPath(value);
    if (!checked.ok) {
      pathError[key] = checked.error;
      return null;
    }
    delete pathError[key];
    return checked.path;
  }
  /** Adds a standing grant; a refusal (the server's reason) shows under the row's `which` input. */
  async function addGrantOf(row: Row, which: 'grant' | 'socket', grant: NewGrant): Promise<boolean> {
    try {
      defaults = (await app.client.request('sandbox.set_defaults', { scope: row.scope, change: { kind: 'add_grant', grant } })).defaults;
      return true;
    } catch (e) {
      pathError[`${row.key}:${which}`] = describeError(e);
      return false;
    }
  }
  async function addGrant(row: Row, typedValue?: string, typedAccess?: PathAccess) {
    const path = typedPath(row, 'grant', typedValue ?? grantPath[row.key] ?? '');
    if (path === null) return;
    const access = typedAccess ?? grantAccess[row.key] ?? 'read';
    const refused = path === '/' && access === 'read_write' ? '/ can’t be granted read-write' : grantLimit(path);
    if (refused) {
      pathError[`${row.key}:grant`] = `${path} can’t be granted: ${refused}`;
      return;
    }
    if (await addGrantOf(row, 'grant', { kind: 'path', value: path, access })) {
      if (typedValue === undefined) grantPath[row.key] = '';
    }
  }
  async function addSocket(row: Row, typedValue?: string) {
    const path = typedPath(row, 'socket', typedValue ?? socketPath[row.key] ?? '');
    if (path === null) return;
    if (await addGrantOf(row, 'socket', { kind: 'socket', value: path, access: null })) {
      if (typedValue === undefined) socketPath[row.key] = '';
    }
  }
  const pathGrants = (p: SandboxDefaults | null) => (p?.grants ?? []).filter((g) => g.kind !== 'socket');
  const socketGrants = (p: SandboxDefaults | null) => (p?.grants ?? []).filter((g) => g.kind === 'socket');
  function addNever(row: Row) {
    const path = typedPath(row, 'never', neverPath[row.key] ?? '');
    if (path === null) return;
    neverPath[row.key] = '';
    void change(row.scope, { kind: 'add_never', path });
  }
  function addCommand(row: Row) {
    const v = (command[row.key] ?? '').trim();
    if (!v) return;
    command[row.key] = '';
    void change(row.scope, { kind: 'set_mediated', mediated: { command: v, action: action[row.key] ?? 'approve' } });
  }
</script>

<div class="small muted" title="The strictest level wins; grants add up">Defaults for new sessions</div>
{#each rows as [label, list] (label)}
  <div class="sec-h">{label}</div>
  {#each list as row (row.key)}
    {@const p = row.defaults}
    {@const paths = pathGrants(p)}
    {@const sockets = socketGrants(p)}
    <div class="card" data-scope={row.key}>
      <div class="h">
        {#if row.color}<span class="pdot" style:background={row.color}></span>{/if}<b>{row.name}</b><span class="sp"></span>
        <Dropdown small label="Sandbox level for {row.label}" value={p?.level ?? 'none'} disabled={!online} options={levelOptions(row)} onchange={(v) => setLevel(row, v)} />
      </div>
      {#if p?.level === 'off'}<div class="off">New sessions here run without a sandbox.</div>{/if}
      {#if paths.length || adding[row.key] === 'grant'}
        <div class="r">
          <span class="l">Grants</span>
          <div class="v">
            {#each paths as g (g.id)}
              <span class="chip-s" data-grant={g.value}><span class="gk">{g.kind}</span><code>{g.value}</code>{#if g.access}<span class="muted small">{g.access.replace('_', '-')}</span>{/if}<button aria-label="Remove grant {g.value} on {row.label}" disabled={!online} onclick={() => change(row.scope, { kind: 'remove_grant', grant_id: g.id })}>×</button></span>
            {/each}
            {#if adding[row.key] === 'grant'}
              <input class="in" placeholder="+ path" title="Absolute or ~/…; a directory includes everything under it" aria-label="Add path grant for {row.label}" aria-invalid={!!pathError[`${row.key}:grant`]} disabled={!online} bind:value={grantPath[row.key]} onkeydown={(e) => onEnter(e, () => void addGrant(row))} />
              <Dropdown small label="Access for {row.label}" value={grantAccess[row.key] ?? 'read'} options={[{ value: 'read', label: 'read' }, { value: 'read_write', label: 'read-write' }]} onchange={(v) => (grantAccess[row.key] = v as PathAccess)} />
            {/if}
            {#if pathError[`${row.key}:grant`]}<span class="err" role="alert">{pathError[`${row.key}:grant`]}</span>{/if}
          </div>
        </div>
      {/if}
      {#if sockets.length || adding[row.key] === 'socket'}
        <div class="r">
          <span class="l" title={SOCKET_NOTE}>Sockets <span class="q" aria-hidden="true">?</span></span>
          <div class="v">
            {#each sockets as g (g.id)}
              <span class="chip-s" data-socket={g.value}><code>{g.value}</code><button aria-label="Remove socket {g.value} on {row.label}" disabled={!online} onclick={() => change(row.scope, { kind: 'remove_grant', grant_id: g.id })}>×</button></span>
            {/each}
            {#if adding[row.key] === 'socket'}
              <input class="in" placeholder="+ socket" title="A Unix socket the sandbox may connect to (absolute or ~/…)" aria-label="Add socket for {row.label}" aria-invalid={!!pathError[`${row.key}:socket`]} disabled={!online} bind:value={socketPath[row.key]} onkeydown={(e) => onEnter(e, () => void addSocket(row))} />
            {/if}
            {#if pathError[`${row.key}:socket`]}<span class="err" role="alert">{pathError[`${row.key}:socket`]}</span>{/if}
          </div>
        </div>
      {/if}
      {#if p?.never.length || adding[row.key] === 'never'}
        <div class="r">
          <span class="l" title="Denied without asking">Never</span>
          <div class="v">
            {#each p?.never ?? [] as n (n)}<span class="chip-s" data-never={n}><code>{n}</code><button aria-label="Remove never-path {n} on {row.label}" disabled={!online} onclick={() => change(row.scope, { kind: 'remove_never', path: n })}>×</button></span>{/each}
            {#if adding[row.key] === 'never'}
              <input class="in" placeholder="+ path" title="Denied without asking; absolute or ~/…" aria-label="Add never-path for {row.label}" aria-invalid={!!pathError[`${row.key}:never`]} disabled={!online} bind:value={neverPath[row.key]} onkeydown={(e) => onEnter(e, () => addNever(row))} />
            {/if}
            {#if pathError[`${row.key}:never`]}<span class="err" role="alert">{pathError[`${row.key}:never`]}</span>{/if}
          </div>
        </div>
      {/if}
      {#if p?.domains.length || adding[row.key] === 'domain'}
        <div class="r">
          <span class="l">Domains</span>
          <div class="v">
            {#each p?.domains ?? [] as d (d)}<span class="chip-s"><code>{d}</code><button aria-label="Remove {d}" disabled={!online} onclick={() => change(row.scope, { kind: 'remove_domain', domain: d })}>×</button></span>{/each}
            {#if adding[row.key] === 'domain'}<input class="in" placeholder="+ domain" aria-label="Add allowed domain for {row.label}" disabled={!online} bind:value={domain[row.key]} onkeydown={(e) => onEnter(e, () => addDomain(row))} />{/if}
          </div>
        </div>
      {/if}
      {#if p?.mediated.length || adding[row.key] === 'command'}
        <div class="r">
          <span class="l">Commands</span>
          <div class="v">
            {#each p?.mediated ?? [] as m (m.command)}<span class="chip-s"><code>{m.command}</code><span class="small">→ {m.action === 'deny' ? 'deny' : 'ask'}</span><button aria-label="Remove {m.command}" disabled={!online} onclick={() => change(row.scope, { kind: 'remove_mediated', command: m.command })}>×</button></span>{/each}
            {#if adding[row.key] === 'command'}
              <input class="in" placeholder="+ command" aria-label="Add mediated command for {row.label}" disabled={!online} bind:value={command[row.key]} onkeydown={(e) => onEnter(e, () => addCommand(row))} />
              <Dropdown small label="Action for {row.label}" value={action[row.key] ?? 'approve'} options={[{ value: 'approve', label: 'ask' }, { value: 'deny', label: 'deny' }]} onchange={(v) => (action[row.key] = v as MediationAction)} />
            {/if}
          </div>
        </div>
      {/if}
      <div class="add"><ActionMenu text="+ rule" label="Add a rule for {row.label}" actions={ruleActions(row, paths, sockets)} /></div>
    </div>
  {/each}
{/each}

<style>
  .card { border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; margin-bottom: 8px; display: flex; flex-direction: column; gap: 6px; }
  .h { display: flex; align-items: center; gap: 8px; }
  .pdot { width: 10px; height: 10px; border-radius: 3px; }
  .off { background: color-mix(in srgb, var(--bad) 13%, var(--bg2)); border-radius: 6px; padding: 6px 8px; font-size: 13px; }
  .r { display: flex; gap: 10px; align-items: baseline; font-size: 13px; }
  .l { width: 130px; flex: none; color: var(--muted); font-size: 12px; }
  .v { display: flex; flex-wrap: wrap; gap: 4px 6px; align-items: center; min-width: 0; }
  .chip-s { display: inline-flex; align-items: center; gap: 4px; border: 1px solid var(--line); border-radius: 6px; padding: 1px 2px 1px 6px; font-size: 12px; }
  .chip-s button { border: 0; background: none; color: var(--muted); cursor: pointer; padding: 0 4px; font-size: 14px; line-height: 1; }
  .chip-s button:hover { color: var(--bad); }
  .gk { font: 650 10.5px var(--mono); text-transform: uppercase; padding: 0 5px; border-radius: 5px; background: var(--bg3); }
  .in { border: 1px dashed var(--line); border-radius: 6px; padding: 2px 6px; font-size: 12px; width: 190px; max-width: 100%; background: transparent; }
  .in[aria-invalid='true'] { border-color: var(--bad); }
  .err { color: var(--bad); font-size: 12px; flex-basis: 100%; }
  .q { font-size: 10px; border: 1px solid var(--line); border-radius: 999px; padding: 0 4px; cursor: help; }
  .add { align-self: flex-start; }
  @media (max-width: 760px) { .r { flex-direction: column; gap: 4px; } }
</style>
