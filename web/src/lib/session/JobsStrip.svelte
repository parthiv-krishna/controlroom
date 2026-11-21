<script lang="ts">
  // The running background jobs (spec §8), one row at most and nothing when none runs: finished
  // jobs live in the timeline (their row) and in the Jobs sheet (the session's ⋯ menu). A few chips
  // (a phone shows one: the job, or "⚙ N running") and "+N" for the rest; any of them opens the
  // Jobs sheet, where a job's log, follow state and Kill are.
  import { useApp } from '../app/app.svelte';
  import { duration } from '../model/status';
  import type { FeedSession, Job } from '../protocol';

  /** Chips shown before "+N" on a wide screen. */
  const CHIPS = 3;

  let { s, jobs }: { s: FeedSession; jobs: Job[] } = $props();
  const app = useApp();
  const running = $derived(jobs.filter((j) => j.status === 'running').sort((a, b) => b.started_at - a.started_at));
  const max = $derived(app.view.wide ? CHIPS : 1);
  // On a phone several running jobs are one chip.
  const shown = $derived(running.length > max && !app.view.wide ? [] : running.slice(0, max));
  const rest = $derived(running.length - shown.length);
  let now = $state(Date.now());
  $effect(() => {
    if (!running.length) return;
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const open = (job: string | null) => (app.view.modal = { kind: 'jobs', session: { id: s.id }, job });
  const wakeIn = (j: Job) => (j.follow?.next_wake_at != null ? Math.max(0, Math.ceil((j.follow.next_wake_at - now) / 1000)) : null);
  const meta = (j: Job) => {
    const w = wakeIn(j);
    return w !== null ? `wake in ${w}s` : duration(now - j.started_at);
  };
</script>

{#if running.length}
  <div class="jobs" aria-label="Running jobs">
    {#each shown as j (j.id)}
      <button class="chip-j" class:following={!!j.follow} title={j.command} aria-label="Job {j.name}, running {meta(j)}" onclick={() => open(j.id)}>
        <span class="spin" aria-hidden="true"></span><span class="n">{j.name}</span><span class="m">{meta(j)}</span>
      </button>
    {/each}
    {#if rest}
      <button class="chip-j more" aria-label="{running.length} jobs running: show all" onclick={() => open(null)}>
        {#if shown.length}+{rest}{:else}<span aria-hidden="true">⚙</span> {running.length} running{/if}
      </button>
    {/if}
  </div>
{/if}

<style>
  /* One row, always: chips never wrap, the row never grows. */
  .jobs { display: flex; align-items: center; gap: 6px; flex-wrap: nowrap; overflow: hidden; height: 26px; flex: none; margin: 6px auto 0; max-width: 820px; width: calc(100% - 80px); font-size: 12px; }
  .chip-j { display: inline-flex; align-items: center; gap: 5px; flex: 0 1 auto; min-width: 0; max-width: 220px; height: 22px; border: 1px solid var(--line); border-radius: 999px; padding: 0 9px; background: var(--bg2); font: inherit; color: var(--fg); cursor: pointer; white-space: nowrap; }
  .chip-j.following { border-color: color-mix(in srgb, var(--info) 45%, var(--line)); }
  .chip-j.more { flex: none; color: var(--muted); }
  .n { overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .m { flex: none; color: var(--muted); font-variant-numeric: tabular-nums; }
  .spin { flex: none; width: 8px; height: 8px; border-radius: 50%; background: var(--info); animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.35; } }
  @media (prefers-reduced-motion: reduce) { .spin { animation: none; } }
  @media (max-width: 760px) { .jobs { width: auto; margin-left: 12px; margin-right: 12px; } }
</style>
