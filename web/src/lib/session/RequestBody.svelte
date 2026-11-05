<script lang="ts">
  // The structured subject of a request: command, path, endpoint, diff, git push, contact, spawn,
  // a command waiting for terminal input (the user types in that shell's terminal; the agent
  // can't), and a privileged command (it runs unsandboxed on the host).
  import type { RequestBody, SessionId } from '../protocol';
  import OpenTerminal from '../term/OpenTerminal.svelte';
  import Diff from '../ui/Diff.svelte';

  let {
    body,
    compact = false,
    session,
    open,
  }: {
    body: RequestBody;
    compact?: boolean;
    /** The request's session: a waiting shell links to its terminal there. */
    session?: SessionId;
    /** Shows the session (before its terminal opens). */
    open?: () => void;
  } = $props();
</script>

{#if body.kind === 'command'}
  <pre class="code">$ {body.command}</pre>
  {#if body.cwd && !compact}<div class="small muted">in <code>{body.cwd}</code></div>{/if}
{:else if body.kind === 'path'}
  <div class="line"><span class="acc">{body.access.replace('_', ' ')}</span> <code>{body.path}</code></div>
{:else if body.kind === 'endpoint'}
  <div class="line"><span class="acc">{body.method}</span> <code>{body.url}</code></div>
{:else if body.kind === 'diff'}
  <div class="line"><code>{body.path}</code></div>
  {#if !compact}
    <Diff old={body.old} next={body.new} />
  {/if}
{:else if body.kind === 'git_push'}
  <pre class="code">{body.command}</pre>
  <div class="small"><span class="muted">remote</span> {body.remote} · <span class="muted">branch</span> <b>{body.branch}</b></div>
  {#if body.force}<div class="warn">⚠ force-push rewrites {body.branch}</div>{/if}
{:else if body.kind === 'contact'}
  <div class="line">contact <b>{body.peer.name}</b></div>
  {#if body.reason}<div class="reason">“{body.reason}”</div>{/if}
{:else if body.kind === 'spawn'}
  <div class="line"><span class="acc">spawn</span> {body.agent} on <b>{body.machine}</b> · {body.title}</div>
  {#if !compact}<div class="small muted">in <code>{body.cwd}</code></div>{/if}
{:else if body.kind === 'terminal_input'}
  <pre class="code">$ {body.command}</pre>
  <div class="line" data-shell={body.shell}>{#if session}<OpenTerminal {session} shell={body.shell} {open} />{:else}<b class="shell">{body.shell}</b>{/if} on <b>{body.machine}</b>: type in its terminal</div>
{:else if body.kind === 'privileged'}
  <pre class="code">$ {body.command}</pre>
  <div class="line"><span class="acc">unsandboxed</span> on <b>{body.machine}</b>{#if body.cwd && !compact} · in <code>{body.cwd}</code>{/if}</div>
  {#if body.reason}<div class="reason">“{body.reason}”</div>{/if}
{/if}

<style>
  .code { background: var(--bg3); border-radius: 6px; padding: 6px 10px; font-size: 12.5px; }
  .line { font-size: 13px; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .acc { font: 650 10.5px var(--mono); text-transform: uppercase; padding: 1px 6px; border-radius: 5px; background: var(--bg3); }
  .warn { color: var(--warn); font-size: 12.5px; }
  .reason { color: var(--muted); font-style: italic; font-size: 13px; }
  .shell { font-family: var(--mono); }
</style>
