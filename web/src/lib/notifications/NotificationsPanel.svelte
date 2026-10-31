<script lang="ts">
  // What hasn't been dealt with in the active project: Needs you (blocking requests), Questions
  // (non-blocking; agents keep working), Failed or stopped, Finished; machines that need the user
  // or can't be reached; "N more in other projects". Clicking a card opens the session.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { displayName } from '../model/filters';
  import { notes, type RequestNote } from '../model/notifications';
  import { keyOf, tasksFor } from '../model/projects';
  import { ago, connLabel, statusKind } from '../model/status';
  import type { MachineSession } from '../protocol';
  import RequestCard from '../session/RequestCard.svelte';
  import MachineFix from '../settings/MachineFix.svelte';
  import StatusIcon from '../ui/StatusIcon.svelte';

  let { overlay = false }: { overlay?: boolean } = $props();
  const app = useApp();
  const n = $derived(notes(app.ctx, app.project));
  const others = $derived(app.project ? notes(app.ctx, null).total - n.total : 0);
  const fixes = $derived(app.client.feed.machineList.filter((m) => m.connectivity.state === 'action_required'));
  const unreachable = $derived(n.offline.filter((o) => o.machine.connectivity.state !== 'action_required'));
  const online = $derived(app.client.online);
  const sections = $derived<[string, string, RequestNote[]][]>([
    ['blocking', 'Needs you', n.blocking],
    ['questions', 'Questions', n.questions],
  ]);

  function openCard(e: MouseEvent, s: MachineSession) {
    if ((e.target as HTMLElement).closest('button, input, textarea, a')) return;
    if (!app.desk) app.view.notesOpen = false;
    app.open(s);
  }
  const isOn = (s: MachineSession) => !!app.view.current && keyOf(app.view.current) === keyOf(s);
</script>

{#snippet head(s: MachineSession)}
  {@const t = tasksFor(app.ctx, s)[0]}
  <div class="ch"><StatusIcon kind={statusKind(s)} /><b>{displayName(s)}</b><span class="t">{ago(s.updated_at)}</span></div>
  <div class="sub">{t ? `${t.title} · ` : ''}{app.client.feed.machines.get(s.machine_id)?.name ?? ''}</div>
{/snippet}

<aside class="np" class:overlay aria-label="Notifications" data-pane="notifications">
  <div class="nh">
    <b>Notifications</b>{#if n.total}<span class="count">{n.total}</span>{/if}<span class="sp"></span>
    {#if !app.desk}<button class="x" aria-label="Close" onclick={() => (app.view.notesOpen = false)}>×</button>{/if}
  </div>
  <div class="nb">
    {#each sections as [kind, label, cards] (kind)}
      {#if cards.length}
        <div class="sec">{label}{#if kind === 'questions'}<span class="hint">agents keep working</span>{/if}</div>
        {#each cards as c (keyOf(c.s) + c.req.id)}
          <div class="card k-{kind}" class:on={isOn(c.s)} role="button" tabindex="0" onclick={(e) => openCard(e, c.s)} onkeydown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) app.open(c.s); }}>
            {@render head(c.s)}
            <RequestCard req={c.req} s={c.s} compact />
          </div>
        {/each}
      {/if}
    {/each}
    {#if n.problems.length}
      <div class="sec">Failed or stopped</div>
      {#each n.problems as s (keyOf(s))}
        <div class="card" class:on={isOn(s)} role="button" tabindex="0" onclick={(e) => openCard(e, s)} onkeydown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) app.open(s); }}>
          {@render head(s)}
          <div class="what">{s.last_error ?? s.preview}</div>
          <div class="acts">
            {#if (s.state === 'interrupted' || s.state === 'dormant') && s.resumable}<button class="btn sm primary" disabled={!online} onclick={() => app.client.call('session.resume', onSession(s))}>Resume</button>{/if}
            {#if s.state === 'interrupted' || s.state === 'failed'}<button class="btn sm" disabled={!online} onclick={() => (app.view.modal = { kind: 'start-fresh', session: { machine_id: s.machine_id, id: s.id } })}>Start fresh</button>{/if}
            <button class="btn sm ghost" disabled={!online} onclick={() => app.markRead(s)}>Dismiss</button>
          </div>
        </div>
      {/each}
    {/if}
    {#if n.finished.length}
      <div class="sec">Finished{#if n.finished.length > 1}<button class="link" disabled={!online} onclick={() => n.finished.forEach((s) => app.markRead(s))}>Mark all read</button>{/if}</div>
      {#each n.finished as s (keyOf(s))}
        <div class="card" class:on={isOn(s)} role="button" tabindex="0" onclick={(e) => openCard(e, s)} onkeydown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) app.open(s); }}>
          {@render head(s)}
          <div class="what">{s.preview}</div>
          <div class="acts"><button class="btn sm ghost" disabled={!online} onclick={() => app.markRead(s)}>Mark read</button></div>
        </div>
      {/each}
    {/if}
    {#each fixes as m (m.id)}
      <div class="card fix">
        <div class="ch"><StatusIcon kind="attention" /><b>{m.name}</b><span class="t">{connLabel(m)}</span></div>
        <MachineFix {m} compact />
      </div>
    {/each}
    {#each unreachable as { machine, waiting } (machine.id)}
      <div class="off">
        <StatusIcon kind="offline" /><span><b>{machine.name}</b> {connLabel(machine)} · {waiting} request{waiting === 1 ? '' : 's'} waiting</span>
        {#if machine.connectivity.state === 'incompatible'}
          <button class="btn sm" disabled={!online} onclick={() => app.client.call('machines.upgrade', { machine_id: machine.id })}>Upgrade</button>
        {:else}
          <button class="btn sm" disabled={!online} onclick={() => app.client.call('machines.reconnect', { machine_id: machine.id })}>Retry</button>
        {/if}
      </div>
    {/each}
    {#if !n.total && !unreachable.length && !fixes.length}<div class="empty-n">All caught up</div>{/if}
  </div>
  {#if others > 0}<button class="foot" onclick={() => app.openProject(null)}>{others} more in other projects →</button>{/if}
</aside>

<style>
  .np { grid-area: notes; display: flex; flex-direction: column; min-height: 0; background: var(--bg); border-left: 1px solid var(--line); }
  .np.overlay { position: fixed; top: 0; right: 0; bottom: 0; width: var(--notes-w, 360px); z-index: 70; box-shadow: -18px 0 50px rgba(0, 0, 0, 0.14); }
  .nh { display: flex; align-items: center; gap: 8px; height: var(--top-h); padding: 0 12px 0 16px; border-bottom: 1px solid var(--line); flex: none; }
  .count { font-size: 11px; color: var(--muted); background: var(--bg3); border-radius: 999px; padding: 0 7px; }
  .x { background: none; border: none; color: var(--muted); font-size: 24px; cursor: pointer; }
  .nb { flex: 1; overflow-y: auto; padding: 4px 10px 52px; }
  .sec { display: flex; align-items: center; gap: 8px; font-size: 11.5px; color: var(--muted); padding: 14px 4px 6px; }
  .hint { font-style: italic; }
  .link { margin-left: auto; background: none; border: none; color: var(--muted); font: inherit; font-size: 11.5px; cursor: pointer; }
  .link:hover { color: var(--fg); text-decoration: underline; }
  .card { background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; padding: 9px 11px; margin-bottom: 7px; cursor: pointer; display: flex; flex-direction: column; gap: 4px; }
  .card:hover { border-color: color-mix(in srgb, var(--muted) 60%, var(--line)); }
  .card.on { border-color: var(--fg); }
  .card.fix { cursor: default; }
  .ch { display: flex; align-items: center; gap: 7px; min-width: 0; }
  .ch b { flex: 1; font-size: 13.5px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .t { font-size: 11px; color: var(--muted); flex: none; }
  .sub { font-size: 11.5px; color: var(--muted); padding-left: 21px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .what { font-size: 13px; padding-left: 21px; display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .acts { display: flex; flex-wrap: wrap; gap: 5px; padding: 3px 0 0 21px; }
  .off { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--muted); padding: 8px 4px; border-top: 1px dashed var(--line); margin-top: 6px; }
  .off span { flex: 1; }
  .off b { color: var(--fg); }
  .empty-n { color: var(--muted); text-align: center; padding: 50px 10px; font-size: 13.5px; }
  .foot { flex: none; background: none; border: none; border-top: 1px solid var(--line); color: var(--muted); font: inherit; font-size: 12.5px; padding: 10px 16px; text-align: left; cursor: pointer; }
  .foot:hover { color: var(--fg); }
  @media (max-width: 760px) {
    .np.overlay { left: 0; width: auto; padding-top: env(safe-area-inset-top); z-index: 85; }
  }
</style>
