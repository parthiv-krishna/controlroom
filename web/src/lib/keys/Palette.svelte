<script lang="ts">
  // The command palette (⌘K / Ctrl-Shift-K, or `:` for ex commands): sessions and commands.
  // Ex commands: :approve, :new, :goto <session>, :task, :profile <name>, :settings [tab], :help.
  // Sessions of every profile are listed (with several, the hint starts with the profile's name);
  // opening one enters its profile.
  import { newOpId, useApp } from '../app/app.svelte';
  import { settingsTab } from '../app/view.svelte';
  import { displayName } from '../model/filters';
  import { profileNamed } from '../model/profiles';
  import { machinesOf } from '../model/workspaces';
  import type { FeedSession } from '../protocol';
  import { autofocus } from '../ui/actions';

  const app = useApp();
  /** The session's machines, by name. */
  const machineNames = (s: FeedSession) => machinesOf(s).map((id) => app.client.feed.machines.get(id)?.name ?? '?').join(', ');
  const view = app.view;
  let q = $state('');
  let sel = $state(0);
  $effect(() => {
    if (view.palette !== null) {
      q = view.palette;
      sel = 0;
    }
  });

  interface Item {
    kind: 'command' | 'session';
    label: string;
    /** Shown after the label, each on its own. */
    hints: string[];
    run: () => void;
  }
  interface Ex {
    cmd: string;
    args?: string;
    what: string;
    run: (arg: string) => void;
  }
  // :profile exists only with several profiles.
  const EX: Ex[] = $derived([
    { cmd: 'approve', what: 'Approve the oldest request', run: () => void approve() },
    { cmd: 'new', what: 'New session', run: () => app.newSession() },
    { cmd: 'task', what: 'New task', run: () => app.newTask() },
    {
      cmd: 'goto',
      args: '<session>',
      what: 'Go to a session',
      run: (a) => {
        const s = app.all.sessions.find((x) => displayName(x).toLowerCase().includes(a.toLowerCase()));
        if (s) app.open(s);
        else app.toasts.show(`No session matches “${a}”`, 'error');
      },
    },
    ...(app.multi
      ? [
          {
            cmd: 'profile',
            args: '<name>',
            what: 'Switch profile',
            run: (a: string) => {
              const p = profileNamed(app.profiles, a);
              if (p) app.openProject(null, p.id);
              else app.toasts.show(`No profile matches “${a}”`, 'error');
            },
          },
        ]
      : []),
    { cmd: 'settings', args: '[tab]', what: 'Open settings', run: (a) => app.openSettings(settingsTab(a || 'machines')) },
    { cmd: 'help', what: 'Keyboard help', run: () => (view.help = true) },
  ]);

  async function approve() {
    const cur = app.current;
    const all = [...(cur ? [cur] : []), ...app.ctx.sessions];
    for (const s of all) {
      const r = s.requests.find((x) => x.blocking && x.state === 'pending' && x.options.some((o) => o.style === 'allow'));
      const opt = r?.options.find((o) => o.style === 'allow');
      if (!r || !opt) continue;
      const id = r.origin_child ?? s.id;
      const ok = await app.client.call('request.respond', { op_id: newOpId(), id, request_id: r.id, response: { kind: 'choice', option_id: opt.id } });
      if (ok) app.toasts.show(`${opt.label}: ${r.title}`, 'ok', { id });
      return;
    }
    app.toasts.show('Nothing to approve');
  }

  const ex = $derived(q.startsWith(':'));
  const items = $derived.by((): Item[] => {
    if (ex) {
      const [c, ...rest] = q.slice(1).split(' ');
      const arg = rest.join(' ').trim();
      return EX.filter((e) => e.cmd.startsWith(c.toLowerCase())).map((e) => ({ kind: 'command', label: `:${e.cmd}${e.args ? ' ' + e.args : ''}`, hints: [e.what], run: () => e.run(arg) }));
    }
    const t = q.trim().toLowerCase();
    const sessions: Item[] = app.all.sessions
      .filter((s) => s.state !== 'closed' && (!t || displayName(s).toLowerCase().includes(t)))
      .slice(0, t ? 8 : 6)
      .map((s) => {
        const profile = app.shownProfile(s.profile_id)?.name;
        return { kind: 'session', label: displayName(s), hints: [...(profile ? [profile] : []), machineNames(s), s.state.replace(/_/g, ' ')], run: () => app.open(s) };
      });
    const commands: Item[] = EX.filter((e) => !t || e.cmd.includes(t) || e.what.toLowerCase().includes(t)).map((e) => ({ kind: 'command', label: e.what, hints: [`:${e.cmd}`], run: () => e.run('') }));
    return [...sessions, ...commands];
  });

  const close = () => (view.palette = null);
  function run(it: Item | undefined) {
    close();
    it?.run();
  }
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) {
      e.preventDefault();
      sel = Math.min(items.length - 1, sel + 1);
    } else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) {
      e.preventDefault();
      sel = Math.max(0, sel - 1);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      run(items[sel]);
    }
  }
</script>

{#if view.palette !== null}
  <div class="bg" role="presentation" onmousedown={(e) => { if (e.target === e.currentTarget) close(); }}>
    <div class="pal" role="dialog" aria-label="Command palette">
      <input bind:value={q} oninput={() => (sel = 0)} onkeydown={onkey} placeholder="Go to session, or : for commands" aria-label="Command" class:ex use:autofocus />
      <div class="items" role="listbox" aria-label="Results">
        {#each items as it, i (it.kind + it.label + i)}
          {#if i === 0 || items[i - 1].kind !== it.kind}<div class="sec">{it.kind === 'session' ? 'Sessions' : 'Commands'}</div>{/if}
          <button class="it" class:on={i === sel} role="option" aria-selected={i === sel} onmousemove={() => (sel = i)} onclick={() => run(it)}>
            <span class="l">{it.label}</span>{#each it.hints as h, j (j)}<span class="h">{h}</span>{/each}
          </button>
        {:else}
          <div class="none">{ex ? 'Unknown command' : 'No matches'}</div>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .bg { position: fixed; inset: 0; z-index: 250; background: rgba(0, 0, 0, 0.25); display: flex; justify-content: center; align-items: flex-start; padding-top: 12vh; }
  .pal { width: min(620px, 94vw); background: var(--bg2); border: 1px solid var(--line); border-radius: 12px; box-shadow: var(--shadow); overflow: hidden; display: flex; flex-direction: column; }
  input { border: none; border-bottom: 1px solid var(--line); border-radius: 0; padding: 12px 14px; font-size: 15px; outline: none; }
  input:focus { outline: none; border-color: var(--line); }
  input.ex { font-family: var(--mono); }
  .items { max-height: 50vh; overflow-y: auto; padding: 4px; }
  .sec { font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.05em; padding: 8px 10px 4px; }
  .it { display: flex; gap: 10px; align-items: center; width: 100%; background: none; border: none; color: var(--fg); font: inherit; text-align: left; padding: 7px 10px; border-radius: 7px; cursor: pointer; }
  .it.on { background: var(--bg3); }
  .l { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .h { font-size: 12px; color: var(--muted); white-space: nowrap; }
  .none { padding: 14px; color: var(--muted); font-size: 13px; }
</style>
