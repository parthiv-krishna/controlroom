<script lang="ts">
  // The session's background jobs (spec §8), running first, then the finished ones, newest first.
  // A job opens in place: its command, log path (copy), follow state (the user can unfollow a noisy
  // job; following stays the agent's decision), Kill while it runs, and its log tail (job.logs,
  // refreshed as lines arrive, at most every few seconds). A bottom sheet on phones.
  import { onSession, useApp } from '../app/app.svelte';
  import { duration } from '../model/status';
  import { onMachine } from '../model/workspaces';
  import type { FeedSession, Job, JobLogs } from '../protocol';
  import Modal from '../ui/Modal.svelte';

  const LOG_REFRESH_MS = 3000;
  const LOG_LINES = 40;

  let { s, jobs, job: initial, onclose }: { s: FeedSession; jobs: Job[]; job: string | null; onclose: () => void } = $props();
  const app = useApp();
  const online = $derived(app.client.online);
  // svelte-ignore state_referenced_locally
  let openId = $state<string | null>(initial);
  const ordered = $derived(
    [...jobs].sort((a, b) => Number(b.status === 'running') - Number(a.status === 'running') || (b.ended_at ?? b.started_at) - (a.ended_at ?? a.started_at)),
  );
  const job = $derived(jobs.find((j) => j.id === openId) ?? null);
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
      void app.client.call('job.logs', { id: s.id, job_id: j.id, max_lines: LOG_LINES }).then((r) => {
        if (r && logsFor === j.id) logs = r;
      });
    }, Math.max(0, fetchedAt + LOG_REFRESH_MS - Date.now()));
    return () => clearTimeout(t);
  });

  const ICON: Record<Job['status'], string> = { running: '', succeeded: '✓', failed: '✕', timed_out: '✕', killed: '✕', lost: '?', unknown: '?' };
  const outcome = (j: Job): string => {
    switch (j.status) {
      case 'running': return duration(now - j.started_at);
      case 'succeeded': return 'done';
      case 'failed': return j.exit !== null ? `exit ${j.exit}` : 'failed';
      case 'timed_out': return 'timed out';
      case 'killed': return j.signal ?? 'killed';
      case 'lost': return 'lost';
      default: return 'ended';
    }
  };
  const took = (j: Job) => duration((j.ended_at ?? now) - j.started_at);
  const wakeIn = (j: Job) => (j.follow?.next_wake_at != null ? Math.max(0, Math.ceil((j.follow.next_wake_at - now) / 1000)) : null);
</script>

<Modal label="Jobs" oncancel={onclose}>
  <div class="hd"><h3>Jobs</h3><button class="btn xs ghost" aria-label="Close jobs" onclick={onclose}>✕</button></div>
  <ul class="list" aria-label="Jobs">
    {#each ordered as j (j.id)}
      <li>
        <button class="row j-{j.status}" aria-expanded={openId === j.id} aria-label="Job {j.name}, {j.status === 'running' ? 'running' : outcome(j)}" onclick={() => (openId = openId === j.id ? null : j.id)}>
          {#if j.status === 'running'}<span class="spin" aria-hidden="true"></span>{:else}<span class="ic" aria-hidden="true">{ICON[j.status]}</span>{/if}
          <span class="n">{j.name}</span>
          <span class="m">{outcome(j)}</span>
        </button>
        {#if openId === j.id && job}
          {@const log = onMachine(job.machine_id, job.log_path, app.client.feed.machines)}
          <div class="detail">
            <code class="cmd" title={job.command}>{job.command}</code>
            <div class="meta">
              <span class="muted" title="{job.lines} lines">{job.status === 'running' ? 'running' : `took ${took(job)}`}</span>
              {#if job.status === 'running'}
                {@const w = wakeIn(job)}
                <span class="follow" class:on={!!job.follow} title={job.follow ? `Wakes the agent on output (after ${job.follow.buffer_seconds}s) and on exit` : 'Wakes the agent on exit'}>{job.follow ? `following${job.follow.pattern ? ` /${job.follow.pattern}/` : ''}` : 'wakes on exit'}</span>
                {#if w !== null}<span class="wake">wake in {w}s</span>{/if}
              {/if}
              <span class="sp"></span>
              {#if job.status === 'running' && job.follow}<button class="btn xs ghost" disabled={!online} onclick={() => app.client.call('job.unfollow', { ...onSession(s), job_id: job.id })}>Unfollow</button>{/if}
              <button class="btn xs ghost" title={log} aria-label="Copy log path {log}" onclick={() => app.copy(log)}>Copy log path</button>
              {#if job.status === 'running'}<button class="btn xs danger" disabled={!online} aria-label="Kill job {job.name}" onclick={() => app.client.call('job.kill', { ...onSession(s), job_id: job.id })}>Kill</button>{/if}
            </div>
            <pre class="log" aria-label="Log tail">{logs ? logs.lines.join('\n') || '(no output yet)' : '…'}</pre>
          </div>
        {/if}
      </li>
    {:else}
      <li class="muted">No jobs yet.</li>
    {/each}
  </ul>
</Modal>

<style>
  .hd { display: flex; align-items: center; justify-content: space-between; }
  h3 { margin: 0; }
  .list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; max-height: 70dvh; overflow: auto; }
  .row { display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 4px; background: none; border: none; border-radius: 6px; font: inherit; font-size: 13px; color: var(--fg); cursor: pointer; text-align: left; }
  .row:hover { background: var(--bg3); }
  .n { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .m { flex: none; color: var(--muted); font-variant-numeric: tabular-nums; }
  .ic { width: 14px; text-align: center; flex: none; }
  .j-succeeded .ic { color: var(--ok); }
  .j-failed .ic, .j-timed_out .ic, .j-killed .ic { color: var(--bad); }
  .spin { flex: none; width: 8px; height: 8px; margin: 0 3px; border-radius: 50%; background: var(--info); }
  .detail { margin: 2px 0 8px 26px; display: flex; flex-direction: column; gap: 6px; font-size: 12px; }
  .cmd { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .sp { flex: 1; }
  .follow { border: 1px solid var(--line); border-radius: 10px; padding: 0 7px; color: var(--muted); }
  .follow.on { color: var(--info); }
  .wake { color: var(--info); font-variant-numeric: tabular-nums; }
  .log { background: var(--bg3); border-radius: 6px; padding: 6px 10px; max-height: 30dvh; overflow: auto; font-size: 12px; margin: 0; }
</style>
