<script lang="ts">
  // One card for every request kind (permission, question, dialog, sandbox, spawn, contact; unknown
  // kinds stay answerable from title, detail and options). Approvals block (the solid flag tier);
  // questions never block (outlined tier): answer or Dismiss ("declined"). A permission request with
  // a deny option offers "Reject and steer…": deny, then send the text ahead of the queue. A sandbox
  // request offers "Deny with reason…": the agent's command fails with that reason.
  // Used in the session pane, the notifications panel and the task detail (compact).
  import { newOpId, useApp } from '../app/app.svelte';
  import { RpcError, describeError } from '../api/errors';
  import { displayName } from '../model/filters';
  import { textContent } from '../model/misc';
  import type { FeedSession, Question, QuestionAnswer, Request, Response } from '../protocol';
  import Icon from '../ui/Icon.svelte';
  import Markdown from '../ui/Markdown.svelte';
  import RequestBody from './RequestBody.svelte';

  let { req, s, compact = false }: { req: Request; s: FeedSession; compact?: boolean } = $props();
  const app = useApp();

  const KIND: Record<Request['kind'], string> = { permission: 'Permission', question: 'Question', dialog: 'Dialog', sandbox: 'Sandbox', contact: 'Contact', spawn: 'Spawn', terminal_input: 'Input', privileged: 'Privileged', unknown: 'Request' };
  // A rolled-up request is answered on the child session.
  const targetId = $derived(req.origin_child ?? s.id);
  const child = $derived(req.origin_child ? app.client.feed.sessions.get(req.origin_child) : undefined);
  const live = $derived(app.client.online);
  const pending = $derived(req.state === 'pending');
  const deny = $derived(req.options.find((o) => o.style === 'deny'));
  const questions = $derived(req.questions);
  /** One single-choice question: picking an option answers at once. */
  const quick = $derived(questions.length === 1 && !questions[0].multi);

  /** The open text box under the options: steer after rejecting, or the reason of a denial. */
  let steering = $state<'steer' | 'reason' | null>(null);
  let steer = $state('');
  /** Per question: the typed answer and the selected option labels. */
  let free = $state<Record<string, string>>({});
  let selected = $state<Record<string, string[]>>({});
  let busy = $state(false);

  /** The answering device's current label, falling back to the label at the time. */
  const deviceLabel = (ref: { device_id: string; label: string }) => app.client.feed.devices.get(ref.device_id)?.label ?? ref.label;

  async function respond(response: Response, then?: string): Promise<boolean> {
    busy = true;
    try {
      await app.client.request('request.respond', {
        op_id: newOpId(),
        id: targetId,
        request_id: req.id,
        response,
        then: then ? textContent(then) : null,
      });
      return true;
    } catch (e) {
      const won = e instanceof RpcError && e.data.kind === 'already_resolved' ? e.data.request.answer?.by : null;
      app.toasts.show(won ? `Already answered on ${deviceLabel(won)}` : describeError(e), 'error');
      return false;
    } finally {
      busy = false;
    }
  }
  const choose = (option_id: string) => respond({ kind: 'choice', option_id });

  /** The answers given so far, one per question that has one. */
  const answers = $derived(
    questions
      .map((q): QuestionAnswer => ({ question_id: q.id, selected: selected[q.id] ?? [], text: free[q.id]?.trim() || null }))
      .filter((a) => a.selected.length || a.text),
  );
  function answer() {
    if (!answers.length) return;
    void respond({ kind: 'answers', answers }).then((ok) => {
      if (ok) {
        free = {};
        selected = {};
      }
    });
  }
  function pickOption(q: Question, label: string) {
    if (quick) return void respond({ kind: 'answers', answers: [{ question_id: q.id, selected: [label], text: null }] });
    const now = selected[q.id] ?? [];
    if (q.multi) selected[q.id] = now.includes(label) ? now.filter((x) => x !== label) : [...now, label];
    else selected[q.id] = now.includes(label) ? [] : [label];
  }
  async function sendSteer() {
    if (!deny || !steer.trim()) return;
    const text = steer.trim();
    const done = steering === 'reason' ? await respond({ kind: 'choice', option_id: deny.id, reason: text }) : await respond({ kind: 'choice', option_id: deny.id, reason: null }, text);
    if (done) {
      steering = null;
      steer = '';
    }
  }
  function steerKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      void sendSteer();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      steering = null;
    }
  }
  function startSteer(mode: 'steer' | 'reason') {
    if (compact) app.open({ id: targetId });
    else steering = mode;
  }
  function freeKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      answer();
    }
  }
</script>

<div class="rq rq-{req.kind}" class:blocking={req.blocking} class:pending class:compact data-request={req.id}>
  <div class="head">
    <span class="flag"><Icon name={req.blocking ? 'flag' : 'flagOutline'} size={13} /></span>
    <span class="kind">{KIND[req.kind]}</span>
    <b class="title">{req.title}</b>
  </div>
  {#if req.origin_child}
    <div class="from">from subagent <button class="linkish" onclick={() => app.open({ id: targetId })}>{child ? displayName(child) : 'subagent'}</button></div>
  {/if}
  {#if req.detail && !compact}<div class="detail"><Markdown text={req.detail} /></div>{/if}
  {#if req.body}<RequestBody body={req.body} {compact} session={targetId} open={() => app.open({ id: targetId })} />{/if}

  {#if pending}
    {#if questions.length}
      {#each questions as q (q.id)}
        {#if !compact && (q.text !== req.title || questions.length > 1)}<div class="detail"><Markdown text={q.text} /></div>{/if}
        {#if !compact || quick}
          <div class="acts">
            {#each q.options as o (o.label)}
              {@const on = (selected[q.id] ?? []).includes(o.label)}
              <button class="btn" class:sm={compact} class:on title={o.description ?? undefined} disabled={!live || busy} onclick={() => pickOption(q, o.label)}>{q.multi ? (on ? '☑ ' : '☐ ') : ''}{o.label}</button>
            {/each}
          </div>
        {/if}
        {#if q.free_text && !compact}
          <input placeholder={q.options.length ? 'Or type an answer…' : 'Type an answer…'} aria-label="Answer" disabled={!live} bind:value={free[q.id]} onkeydown={freeKey} />
        {/if}
      {/each}
      <div class="acts end">
        {#if !req.blocking}<button class="btn ghost" class:sm={compact} title="Answers “declined”; the agent decides on its own" disabled={!live || busy} onclick={() => respond({ kind: 'dismiss' })}>Dismiss</button>{/if}
        {#if compact && !quick}
          <button class="btn sm" onclick={() => app.open({ id: targetId })}>Answer…</button>
        {:else if !compact && (!quick || questions.some((q) => q.free_text))}
          <button class="btn primary" disabled={!live || busy || !answers.length} onclick={answer}><span class="kbd">↵</span> Answer</button>
        {/if}
      </div>
    {:else}
      <div class="acts">
        {#each req.options as o (o.id)}
          <button class="btn" class:sm={compact} class:primary={o.style === 'allow'} class:danger={o.style === 'deny'} disabled={!live || busy} onclick={() => choose(o.id)}>{o.label}</button>
        {/each}
        {#if req.kind === 'permission' && deny && live && !steering}<button class="btn danger" class:sm={compact} onclick={() => startSteer('steer')}>Reject and steer…</button>{/if}
        {#if req.kind === 'sandbox' && deny && live && !steering}<button class="btn danger" class:sm={compact} onclick={() => startSteer('reason')}>{deny.label} with reason…</button>{/if}
        {#if !live}<span class="small warn-t">not connected</span>{/if}
      </div>
      {#if steering}
        <div class="steer">
          <!-- svelte-ignore a11y_autofocus -->
          {#if steering === 'reason'}
            <textarea rows="2" placeholder="Why? The agent sees the reason." aria-label="Reason" bind:value={steer} onkeydown={steerKey} autofocus></textarea>
          {:else}
            <textarea rows="2" placeholder="What should the agent do instead?" aria-label="Steer" bind:value={steer} onkeydown={steerKey} autofocus></textarea>
          {/if}
          <div class="acts end">
            <button class="btn" onclick={() => (steering = null)}><span class="kbd">Esc</span> Cancel</button>
            <button class="btn danger" disabled={!steer.trim() || busy} onclick={sendSteer}><span class="kbd">↵</span> {steering === 'reason' ? deny?.label : 'Reject and steer'}</button>
          </div>
        </div>
      {/if}
    {/if}
  {:else}
    <div class="res">
      {#if req.state === 'answered' && req.answer}
        → {req.answer.label}{#if req.answer.by}<span class="muted">{' · '}{deviceLabel(req.answer.by)}</span>{/if}
      {:else if req.state === 'withdrawn'}⊘ withdrawn by the agent
      {:else}expired{/if}
    </div>
  {/if}
</div>

<style>
  .rq { border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; background: var(--bg2); }
  .rq.pending.blocking { border-color: var(--perm-line); background: var(--perm-bg); }
  .rq.pending:not(.blocking) { border: 1.5px dashed var(--warn); background: none; }
  .rq:not(.pending) { opacity: 0.8; }
  .compact { padding: 8px 10px; font-size: 12.5px; }
  .head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .flag { color: var(--warn); }
  .kind { font-size: 10.5px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); }
  .title { font-size: 13.5px; }
  .compact .title { font-size: 13px; }
  .from { font-size: 12px; color: var(--muted); }
  .detail { font-size: 13px; color: var(--muted); }
  .acts { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .acts.end { justify-content: flex-end; }
  .steer { display: flex; flex-direction: column; gap: 6px; }
  .res { font-size: 12.5px; }
  @media (max-width: 760px) { .acts .btn { padding: 9px 13px; } }
</style>
