<script lang="ts">
  // One card for every request kind (permission, question, dialog, sandbox, spawn, contact; unknown
  // kinds stay answerable from title, detail and options). Approvals block (the solid flag tier);
  // questions never block (outlined tier): answer or Dismiss ("declined"). A blocking request with
  // a deny option offers "Reject and steer…": deny, then send the text ahead of the queue.
  // Used in the session pane, the notifications panel and the task detail (compact).
  import { useApp } from '../app/app.svelte';
  import { RpcError, describeError } from '../api/errors';
  import { displayName } from '../model/filters';
  import { textContent } from '../model/misc';
  import { sessionKey } from '../model/projects';
  import type { MachineSession, QuestionAnswer, Request, Response } from '../protocol';
  import Icon from '../ui/Icon.svelte';
  import RequestBody from './RequestBody.svelte';

  let { req, s, compact = false }: { req: Request; s: MachineSession; compact?: boolean } = $props();
  const app = useApp();

  const KIND: Record<Request['kind'], string> = { permission: 'Permission', question: 'Question', dialog: 'Dialog', sandbox: 'Sandbox', contact: 'Contact', spawn: 'Spawn', unknown: 'Request' };
  // A rolled-up request is answered on the child session (same machine).
  const targetId = $derived(req.origin_child ?? s.id);
  const child = $derived(req.origin_child ? app.client.feed.sessions.get(sessionKey(s.machine_id, req.origin_child)) : undefined);
  const live = $derived(app.client.online && !s.stale);
  const pending = $derived(req.state === 'pending');
  const deny = $derived(req.options.find((o) => o.style === 'deny'));
  const q = $derived(req.questions[0] ?? null);

  let steering = $state(false);
  let steer = $state('');
  let free = $state('');
  let selected = $state<string[]>([]);
  let busy = $state(false);

  /** The answering device's current label, falling back to the label at the time. */
  const deviceLabel = (ref: { device_id: string; label: string }) => app.client.feed.devices.get(ref.device_id)?.label ?? ref.label;

  async function respond(response: Response, then?: string): Promise<boolean> {
    busy = true;
    try {
      await app.client.request('request.respond', {
        machine_id: s.machine_id,
        op_id: crypto.randomUUID(),
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

  function answer() {
    if (!q) return;
    const a: QuestionAnswer = { question_id: q.id, selected, text: free.trim() || null };
    if (!selected.length && !a.text) return;
    void respond({ kind: 'answers', answers: [a] }).then((ok) => {
      if (ok) {
        free = '';
        selected = [];
      }
    });
  }
  function pickOption(label: string) {
    if (!q) return;
    if (q.multi) selected = selected.includes(label) ? selected.filter((x) => x !== label) : [...selected, label];
    else void respond({ kind: 'answers', answers: [{ question_id: q.id, selected: [label], text: null }] });
  }
  async function sendSteer() {
    if (!deny || !steer.trim()) return;
    if (await respond({ kind: 'choice', option_id: deny.id, reason: null }, steer.trim())) {
      steering = false;
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
      steering = false;
    }
  }
  function startSteer() {
    if (compact) app.open({ machine_id: s.machine_id, id: targetId });
    else steering = true;
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
    <div class="from">from subagent <button class="linkish" onclick={() => app.open({ machine_id: s.machine_id, id: targetId })}>{child ? displayName(child) : 'subagent'}</button></div>
  {/if}
  {#if req.detail && !compact}<div class="detail">{req.detail}</div>{/if}
  {#if req.body}<RequestBody body={req.body} {compact} />{/if}

  {#if pending}
    {#if q}
      {#if !compact && q.text !== req.title}<div class="detail">{q.text}</div>{/if}
      <div class="acts">
        {#if !req.blocking}<button class="btn ghost" class:sm={compact} title="Answers “declined”; the agent decides on its own" disabled={!live || busy} onclick={() => respond({ kind: 'dismiss' })}>Dismiss</button>{/if}
        {#each q.options as o (o.label)}
          <button class="btn" class:sm={compact} class:on={selected.includes(o.label)} title={o.description ?? undefined} disabled={!live || busy} onclick={() => pickOption(o.label)}>{q.multi ? (selected.includes(o.label) ? '☑ ' : '☐ ') : ''}{o.label}</button>
        {/each}
      </div>
      {#if q.free_text && !compact}
        <input placeholder={q.options.length ? 'Or type an answer…' : 'Type an answer…'} aria-label="Answer" disabled={!live} bind:value={free} onkeydown={freeKey} />
      {/if}
      {#if !compact && (q.multi || q.free_text)}
        <div class="acts end"><button class="btn primary" disabled={!live || busy || (!free.trim() && !selected.length)} onclick={answer}><span class="kbd">↵</span> Answer</button></div>
      {/if}
    {:else}
      <div class="acts">
        {#each req.options as o (o.id)}
          <button class="btn" class:sm={compact} class:primary={o.style === 'allow'} class:danger={o.style === 'deny'} disabled={!live || busy} onclick={() => choose(o.id)}>{o.label}</button>
        {/each}
        {#if req.blocking && deny && live && !steering}<button class="btn danger" class:sm={compact} onclick={startSteer}>Reject and steer…</button>{/if}
        {#if !live}<span class="small warn-t">{s.stale ? 'machine offline' : 'not connected'}</span>{/if}
      </div>
      {#if steering}
        <div class="steer">
          <!-- svelte-ignore a11y_autofocus -->
          <textarea rows="2" placeholder="What should the agent do instead?" aria-label="Steer" bind:value={steer} onkeydown={steerKey} autofocus></textarea>
          <div class="acts end">
            <button class="btn" onclick={() => (steering = false)}><span class="kbd">Esc</span> Cancel</button>
            <button class="btn danger" disabled={!steer.trim() || busy} onclick={sendSteer}><span class="kbd">↵</span> Reject and steer</button>
          </div>
        </div>
      {/if}
    {/if}
  {:else}
    <div class="res">
      {#if req.state === 'answered' && req.answer}
        → {req.answer.label}{#if req.answer.by}<span class="muted"> · {deviceLabel(req.answer.by)}</span>{/if}
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
  .detail { font-size: 13px; color: var(--muted); white-space: pre-wrap; }
  .acts { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .acts.end { justify-content: flex-end; }
  .steer { display: flex; flex-direction: column; gap: 6px; }
  .res { font-size: 12.5px; }
  @media (max-width: 760px) { .acts .btn { padding: 9px 13px; } }
</style>
