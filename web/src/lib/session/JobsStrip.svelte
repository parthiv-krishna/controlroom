<script lang="ts">
  // Background jobs (spec §8): output goes to a log file the agent reads, never into the context.
  // The exit wakes the agent; following also wakes it on new output after a buffer. The user can
  // unfollow a noisy job and kill one; the open job's log tail is read with job.logs and refreshed
  // as lines arrive (at most every few seconds). A timeline job item opens its job here too.
  import { useApp } from '../app/app.svelte';
  import { onSession } from '../app/app.svelte';
  import { keyOf } from '../model/projects';
  import { duration } from '../model/status';
  import type { Job, JobLogs, MachineSession } from '../protocol';
  import StatusIcon from '../ui/StatusIcon.svelte';

  const LOG_REFRESH_MS = 3000;
  const LOG_LINES = 40;

  let { s, jobs }: { s: MachineSession; jobs: Job[] } = $props();
  const app = useApp();
  const key = $derived(keyOf(s));
  const openId = $derived(app.view.jobOpen[key] ?? null);
  const job = $derived(jobs.find((j) => j.id === openId) ?? null);
  const online = $derived(app.client.online && !s.stale);
  let logs = $state<JobLogs | null>(null);
  let logsFor: string | null = null;
  let fetchedAt = 0;
  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });
  $effect(() => {
    const j = job;
    if (!j || !online) return;
    void j.lines;
    void j.status;
    if (logsFor !== j.id) {
      logs = null;
      logsFor = j.id;
      fetchedAt = 0;
    }
    const t = setTimeout(() => {
      fetchedAt = Date.now();
      void app.client.call('job.logs', { machine_id: s.machine_id, id: s.id, job_id: j.id, max_lines: LOG_LINES }).then((r) => {
        if (r && logsFor === j.id) logs = r;
      });
    }, Math.max(0, fetchedAt + LOG_REFRESH_MS - Date.now()));
    return () => clearTimeout(t);
  });
  const toggle = (id: string) => (app.view.jobOpen[key] = openId === id ? null : id);

  const STATUS: Record<Job['status'], string> = { running: 'running', succeeded: 'succeeded', failed: 'failed', timed_out: 'timed out', killed: 'killed', unknown: '?' };
  const kind = (j: Job) => (j.status === 'running' ? 'working' : j.status === 'succeeded' ? 'complete' : j.status === 'timed_out' ? 'idle' : 'stopped');
  const took = (j: Job) => duration((j.ended_at ?? now) - j.started_at);
  const wakeIn = (j: Job) => (j.follow?.next_wake_at != null ? Math.max(0, Math.ceil((j.follow.next_wake_at - now) / 1000)) : null);
  const followText = (j: Job) => (j.follow ? `following${j.follow.pattern ? ` /${j.follow.pattern}/` : ''} · ${j.follow.buffer_seconds}s` : 'not followed (exit wake only)');
</script>

<div class="jobs" aria-label="Background jobs">
  <span class="l">Jobs</span>
  {#each jobs as j (j.id)}
    {@const w = wakeIn(j)}
    <button class="chip-j j-{j.status}" class:on={openId === j.id} class:following={!!j.follow} title="{j.command} · {STATUS[j.status]}" onclick={() => toggle(j.id)}>
      <StatusIcon kind={kind(j)} /><span>{j.name}</span>
      <span class="m">{w !== null ? `wake in ${w}s` : j.status === 'running' ? took(j) : j.exit !== null ? `exit ${j.exit}` : (j.signal ?? STATUS[j.status])}</span>
    </button>
  {/each}
</div>
{#if job}
  <div class="detail">
    <div class="dh">
      <code>{job.command}</code><span class="sp"></span>
      <span class="muted small">{STATUS[job.status]}{job.exit !== null ? ` · exit ${job.exit}` : ''}{job.signal ? ` · ${job.signal}` : ''}{job.timeout_s ? ` · timeout ${duration(job.timeout_s * 1000)}` : ''} · {took(job)} · {job.lines} lines</span>
      {#if job.status === 'running'}<button class="btn xs danger" disabled={!online} onclick={() => app.client.call('job.kill', { ...onSession(s), job_id: job.id })}>Kill</button>{/if}
      <button class="btn xs ghost" aria-label="Close" onclick={() => (app.view.jobOpen[key] = null)}>✕</button>
    </div>
    <div class="meta">
      {#if job.status === 'running'}
        <span class="follow" class:on={!!job.follow} title={job.follow ? 'Wakes the agent on new output and on exit' : 'Wakes the agent only on exit'}>{followText(job)}</span>
        {#if wakeIn(job) !== null}<span class="wake">next wake in {wakeIn(job)}s</span>{/if}
        {#if job.follow}<button class="btn xs ghost" disabled={!online} title="Stop waking the agent on output; it still wakes on exit" onclick={() => app.client.call('job.unfollow', { ...onSession(s), job_id: job.id })}>Unfollow</button>{/if}
      {/if}
      <span class="sp"></span>
      <code class="path" title={job.log_path}>{job.log_path}</code>
      <button class="btn xs" onclick={() => app.copy(job.log_path)}>Copy</button>
    </div>
    <pre class="log" aria-label="Log tail">{logs ? logs.lines.join('\n') || '(no output yet)' : '…'}</pre>
  </div>
{/if}

<style>
  .jobs { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin: 8px auto 0; max-width: 820px; width: calc(100% - 80px); font-size: 12px; }
  .l { color: var(--muted); font-weight: 600; }
  .chip-j { display: inline-flex; align-items: center; gap: 5px; border: 1px solid var(--line); border-radius: 999px; padding: 2px 9px; background: var(--bg2); font: inherit; color: var(--fg); cursor: pointer; }
  .chip-j.on { border-color: var(--fg); }
  .chip-j.following { border-color: color-mix(in srgb, var(--info) 45%, var(--line)); }
  .m { color: var(--muted); font-variant-numeric: tabular-nums; }
  .detail { margin: 6px auto 0; max-width: 820px; width: calc(100% - 80px); border: 1px solid var(--line); border-radius: 10px; padding: 8px 10px; background: var(--bg2); }
  .dh { display: flex; align-items: center; gap: 8px; font-size: 12px; }
  .dh code { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; font-size: 12px; margin: 4px 0 6px; }
  .follow { border: 1px solid var(--line); border-radius: 10px; padding: 0 7px; color: var(--muted); }
  .follow.on { color: var(--info); }
  .wake { color: var(--info); font-variant-numeric: tabular-nums; }
  .path { font-size: 11.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 420px; }
  .log { background: var(--bg3); border-radius: 6px; padding: 6px 10px; max-height: 160px; overflow: auto; font-size: 12px; }
  @media (max-width: 760px) { .jobs, .detail { width: auto; margin-left: 12px; margin-right: 12px; } }
</style>
