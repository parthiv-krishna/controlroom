<script lang="ts">
  // The structured subject of a request: command, path, endpoint, diff, git push, contact, spawn.
  import { lineDiff } from '../model/misc';
  import type { RequestBody } from '../protocol';

  let { body, compact = false }: { body: RequestBody; compact?: boolean } = $props();
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
    <div class="diff" role="figure" aria-label="Diff">
      {#each lineDiff(body.old, body.new) as l, i (i)}<span class="dl {l.kind}">{l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '} {l.text}</span>{/each}
    </div>
  {/if}
{:else if body.kind === 'git_push'}
  <pre class="code">{body.command}</pre>
  <div class="small"><span class="muted">remote</span> {body.remote} · <span class="muted">branch</span> <b>{body.branch}</b></div>
  {#if body.force}<div class="warn">⚠ force-push: rewrites the history of {body.branch} on the remote</div>{/if}
{:else if body.kind === 'contact'}
  <div class="line">contact <b>{body.peer.name}</b></div>
  {#if body.reason}<div class="reason">“{body.reason}”</div>{/if}
{:else if body.kind === 'spawn'}
  <div class="line"><span class="acc">spawn</span> {body.agent} on <b>{body.machine}</b> · {body.title}</div>
  {#if !compact}<div class="small muted">in <code>{body.cwd}</code></div>{/if}
{/if}

<style>
  .code { background: var(--bg3); border-radius: 6px; padding: 6px 10px; font-size: 12.5px; }
  .line { font-size: 13px; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .acc { font: 650 10.5px var(--mono); text-transform: uppercase; padding: 1px 6px; border-radius: 5px; background: var(--bg3); }
  .diff { background: var(--bg3); border-radius: 6px; padding: 6px 0; overflow: auto; max-height: 320px; font: 12.5px var(--mono); }
  .dl { display: block; padding: 0 10px; white-space: pre; }
  .dl.add { background: color-mix(in srgb, var(--ok) 15%, transparent); color: var(--dadd); }
  .dl.del { background: color-mix(in srgb, var(--bad) 15%, transparent); color: var(--ddel); }
  .dl.ctx { color: var(--muted); }
  .warn { color: var(--warn); font-size: 12.5px; }
  .reason { color: var(--muted); font-style: italic; font-size: 13px; }
</style>
