<script lang="ts">
  // The shared todo list (spec §8): the agent and the user both edit it; each entry records who
  // changed it last. Auto-continue restarts the agent while open entries remain and pauses on its
  // own (only blocked entries left, stalled, budget used up, or a user cancel).
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { tasksFor } from '../model/projects';
  import type { FeedSession, SessionDetail, Todo, TodoStatus } from '../protocol';
  import { dismissable } from '../ui/actions';

  let { s, detail }: { s: FeedSession; detail: SessionDetail } = $props();
  const app = useApp();
  const key = $derived('todo:' + s.id);
  const open = $derived(app.view.panels[key] ?? true);
  const online = $derived(app.client.online);
  const ac = $derived(detail.auto_continue);
  const counted = $derived(detail.todos.filter((t) => t.status !== 'skipped'));
  const done = $derived(counted.filter((t) => t.status === 'done').length);
  const seedFrom = $derived(tasksFor(app.ctx, s).filter((t) => t.checklist.length));

  const STATUS: [TodoStatus, string, string][] = [
    ['pending', '○', 'Pending'], ['in_progress', '◐', 'In progress'], ['done', '●', 'Done'], ['blocked', '⊘', 'Blocked'], ['skipped', '–', 'Skipped'],
  ];
  const AC: Record<string, string> = { on: 'on', off: 'off', paused: 'paused', stalled: 'stalled', budget: 'budget used', unknown: '?' };
  const icon = (st: TodoStatus) => STATUS.find(([v]) => v === st)?.[1] ?? '?';

  let menu = $state<string | null>(null);
  const anchors: Record<string, HTMLButtonElement> = $state({});
  let editing = $state<string | null>(null);
  let draft = $state('');
  let adding = $state('');
  let dragged: string | null = null;

  /** Who changed the entry last, when not the agent: a device (its current label) or the system. */
  function author(t: Todo): string {
    const by = t.updated_by;
    if (by.kind === 'user') return by.device ? (app.client.feed.devices.get(by.device.device_id)?.label ?? by.device.label) : 'user';
    return by.kind === 'system' ? 'system' : '?';
  }
  const update = (t: Todo, fields: { text?: string; status?: TodoStatus; reason?: string }) =>
    app.client.call('todo.update', { ...onSession(s), todo_id: t.id, ...fields });
  const reorder = (todo_id: string, to_index: number) => app.client.call('todo.reorder', { ...onSession(s), todo_id, to_index });
  const setAuto = (fields: { auto_continue?: boolean; auto_continue_budget?: number }) => app.client.call('session.update', { ...onSession(s), ...fields });

  async function setStatus(t: Todo, status: TodoStatus) {
    menu = null;
    if (status !== 'blocked') return void update(t, { status });
    const reason = await app.dialogs.ask({ title: 'Blocked on what?', value: t.reason ?? '', ok: 'Mark blocked' });
    if (reason) void update(t, { status, reason });
  }
  function startEdit(t: Todo) {
    editing = t.id;
    draft = t.text;
  }
  function editKey(e: KeyboardEvent, t: Todo) {
    if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      if (draft.trim() && draft.trim() !== t.text) void update(t, { text: draft.trim() });
      editing = null;
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      editing = null;
    }
  }
  function addKey(e: KeyboardEvent) {
    if (e.key !== 'Enter' || e.isComposing || !adding.trim()) return;
    e.preventDefault();
    e.stopPropagation();
    void app.client.call('todo.add', { ...onSession(s), texts: [adding.trim()] });
    adding = '';
  }
  async function raiseBudget() {
    const v = await app.dialogs.ask({ title: 'Auto-continue budget', value: String(ac.budget * 2), hint: `${ac.runs_used} of ${ac.budget} runs used`, ok: 'Set budget' });
    const n = Number(v);
    if (v && Number.isInteger(n) && n > 0) void setAuto({ auto_continue_budget: n });
  }
</script>

<div class="todo" data-pane="todo" aria-label="Todo">
  <div class="th">
    <button class="fold" aria-expanded={open} onclick={() => (app.view.panels[key] = !open)}>{open ? '▾' : '▸'} <b>Todo</b> <span class="muted">{done}/{counted.length}</span></button>
    <span class="bar"><span style:width="{counted.length ? (done / counted.length) * 100 : 0}%"></span></span>
    <span class="sp"></span>
    <span class="ac ac-{ac.status}" title={ac.reason ?? 'Runs again while todos remain'}>
      Auto-continue <b>{AC[ac.status]}</b>{#if ac.status === 'on' || ac.status === 'budget'}<span class="muted"> · {ac.runs_used}/{ac.budget}</span>{/if}
    </span>
    {#if ac.status === 'off'}<button class="btn xs" disabled={!online} onclick={() => setAuto({ auto_continue: true })}>Turn on</button>
    {:else if ac.status === 'on'}<button class="btn xs ghost" disabled={!online} onclick={() => setAuto({ auto_continue: false })}>Turn off</button>
    {:else if ac.status === 'budget'}<button class="btn xs primary" disabled={!online} onclick={raiseBudget}>Raise budget…</button>
    {:else}<button class="btn xs primary" disabled={!online} onclick={() => setAuto({ auto_continue: true })}>Resume</button>{/if}
  </div>
  {#if ac.reason && ac.status !== 'off' && ac.status !== 'budget'}<div class="why ac-{ac.status}">{ac.status === 'on' ? '◷ ' : ''}{ac.reason}</div>{/if}
  {#if open}
    <ul>
      {#each detail.todos as t, i (t.id)}
        <li class="s-{t.status}" draggable={editing !== t.id} ondragstart={() => (dragged = t.id)} ondragover={(e) => e.preventDefault()} ondrop={(e) => { e.preventDefault(); if (dragged && dragged !== t.id) void reorder(dragged, i); dragged = null; }}>
          <span class="stw">
            <button bind:this={anchors[t.id]} class="stb s-{t.status}" title="Status" aria-label="Status: {t.status}" aria-haspopup="menu" aria-expanded={menu === t.id} disabled={!online} onclick={() => (menu = menu === t.id ? null : t.id)}>{icon(t.status)}</button>
            {#if menu === t.id}
              <span class="menu" role="menu" use:dismissable={{ close: () => (menu = null), anchor: anchors[t.id] }}>
                {#each STATUS as [v, ic, label] (v)}<button role="menuitem" class:on={t.status === v} onclick={() => setStatus(t, v)}><span class="ic s-{v}">{ic}</span> {label}</button>{/each}
              </span>
            {/if}
          </span>
          {#if editing === t.id}
            <!-- svelte-ignore a11y_autofocus -->
            <input class="edit" aria-label="Edit todo" bind:value={draft} onkeydown={(e) => editKey(e, t)} onblur={() => (editing = null)} autofocus />
          {:else}
            <button class="text" disabled={!online} onclick={() => startEdit(t)}>{t.text}</button>
          {/if}
          {#if t.reason && (t.status === 'blocked' || t.status === 'skipped')}<span class="reason" title={t.reason}>{t.reason}</span>{/if}
          {#if t.updated_by.kind !== 'agent'}<span class="by">{author(t)}</span>{/if}
          <span class="acts">
            <button class="btn xs ghost" aria-label="Move up" disabled={!online || i === 0} onclick={() => reorder(t.id, i - 1)}>↑</button>
            <button class="btn xs ghost" aria-label="Move down" disabled={!online || i === detail.todos.length - 1} onclick={() => reorder(t.id, i + 1)}>↓</button>
            <button class="btn xs ghost" aria-label="Remove" disabled={!online} onclick={() => app.client.call('todo.remove', { ...onSession(s), todo_id: t.id })}>✕</button>
          </span>
        </li>
      {/each}
    </ul>
    <div class="add">
      <input placeholder="+ todo" aria-label="Add todo" disabled={!online} bind:value={adding} onkeydown={addKey} />
      {#each seedFrom as t (t.id)}
        <button class="btn xs" disabled={!online} title="Add the checklist of “{t.title}”" onclick={() => app.client.call('todo.add', { ...onSession(s), texts: t.checklist.filter((c) => !c.done).map((c) => c.text) })}>+ checklist</button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .todo { margin: 0 auto; max-width: 820px; width: calc(100% - 80px); border-top: 1px solid var(--line); padding: 6px 0 4px; font-size: 13px; }
  .th { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .fold { background: none; border: none; font: inherit; color: var(--fg); cursor: pointer; padding: 2px 0; display: inline-flex; gap: 6px; align-items: center; }
  .bar { width: 80px; height: 4px; border-radius: 3px; background: var(--bg3); overflow: hidden; }
  .bar span { display: block; height: 100%; background: var(--ok); }
  .ac { font-size: 12px; color: var(--muted); }
  .ac b { font-weight: 650; }
  .ac-on b { color: var(--ok); }
  .ac-paused b, .ac-budget b { color: var(--warn); }
  .ac-stalled b { color: var(--bad); }
  .why { font-size: 12px; margin: 4px 0 2px; color: var(--warn); }
  .why.ac-on { color: var(--muted); }
  .why.ac-stalled { color: var(--bad); }
  ul { list-style: none; margin: 4px 0; padding: 0; max-height: 220px; overflow: auto; }
  li { display: flex; align-items: center; gap: 6px; padding: 2px 4px; border-radius: 7px; }
  li:hover { background: var(--hover); }
  .stw { position: relative; }
  .stb { width: 22px; height: 22px; display: grid; place-items: center; background: none; border: 1px solid transparent; border-radius: 6px; cursor: pointer; font: inherit; color: var(--muted); padding: 0; }
  .stb:hover:not(:disabled) { border-color: var(--line); }
  .s-in_progress { color: var(--info); }
  .s-done { color: var(--ok); }
  .s-blocked { color: var(--bad); }
  .menu { position: absolute; z-index: 40; top: 24px; left: 0; display: flex; flex-direction: column; min-width: 150px; background: var(--bg2); border: 1px solid var(--line); border-radius: 9px; box-shadow: var(--shadow); padding: 4px; }
  .menu button { display: flex; gap: 8px; align-items: center; background: none; border: none; font: inherit; color: var(--fg); padding: 5px 8px; border-radius: 6px; cursor: pointer; text-align: left; }
  .menu button:hover, .menu button.on { background: var(--hover); }
  .ic { width: 14px; text-align: center; }
  .text { flex: 1; min-width: 0; text-align: left; background: none; border: 1px solid transparent; border-radius: 6px; padding: 1px 4px; font: inherit; color: var(--fg); cursor: text; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .text:hover:not(:disabled) { border-color: var(--line); }
  li.s-done .text, li.s-skipped .text { color: var(--muted); text-decoration: line-through; }
  .edit { flex: 1; padding: 1px 4px; }
  .reason { font-size: 11.5px; color: var(--bad); max-width: 40%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .by { font-size: 10.5px; padding: 0 6px; border-radius: 999px; border: 1px solid var(--line); color: var(--muted); white-space: nowrap; }
  .acts { display: inline-flex; opacity: 0; }
  li:hover .acts, li:focus-within .acts { opacity: 1; }
  .add { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .add input { flex: 1; min-width: 160px; padding: 3px 8px; }
  @media (max-width: 760px) { .todo { width: auto; margin: 0 12px; } .acts { opacity: 1; } }
</style>
