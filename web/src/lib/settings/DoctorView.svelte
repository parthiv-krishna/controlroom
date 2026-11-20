<script lang="ts">
  // A machine's doctor report: persistence, boot start, nono and Landlock (and agents, for a report
  // that lists them; host daemons run none: pi runs on the server). When every check passes it is one
  // line, "✓ healthy", which opens the details.
  import type { CheckStatus, DoctorCheck, DoctorReport } from '../protocol';

  let { doctor }: { doctor: DoctorReport } = $props();
  const rows = $derived<[string, DoctorCheck][]>([
    ['Survives logout', doctor.persistence],
    ['Start at boot', doctor.boot_start_check],
    ['Sandbox (nono)', doctor.nono],
  ]);
  const icon = (s: CheckStatus) => (s === 'ok' ? '✓' : s === 'fail' ? '✕' : '!');
  const healthy = $derived(!doctor.agents_config_error && rows.every(([, c]) => c.status === 'ok') && !!doctor.landlock_abi && (!doctor.agents.length || doctor.agents.some((a) => a.status === 'ok')));
  let open = $state(false);
</script>

{#if healthy}<button class="ok-line" aria-expanded={open} onclick={() => (open = !open)}>✓ healthy</button>{/if}
{#if !healthy || open}
<div class="doctor">
  {#if doctor.agents_config_error}<div class="r fail"><span>✕</span><b>agents.toml</b><span class="muted">{doctor.agents_config_error}</span></div>{/if}
  {#each rows as [label, c] (label)}
    <div class="r {c.status}"><span>{icon(c.status)}</span><b>{label}</b><span class="muted">{c.value ?? ''}{#if c.value && c.detail}{' '}({c.detail}){:else}{c.detail}{/if}</span></div>
  {/each}
  <div class="r {doctor.landlock_abi ? 'ok' : 'warn'}"><span>{doctor.landlock_abi ? '✓' : '!'}</span><b>Landlock</b><span class="muted">{doctor.landlock_abi ? `ABI ${doctor.landlock_abi}` : 'not available'}</span></div>
  {#each doctor.agents as a (a.agent)}
    <div class="r {a.status === 'ok' ? 'ok' : 'info'}" title={a.detail}><span>{a.status === 'ok' ? '✓' : '–'}</span><b>{a.agent}</b><span class="muted">{a.status === 'ok' ? (a.path ?? a.command) : a.status === 'missing' ? 'not installed' : "couldn't check"}</span></div>
  {/each}
  {#if doctor.agents.length && !doctor.agents.some((a) => a.status === 'ok')}<div class="note" role="note">No agents available: install {doctor.agents.map((a) => a.command).join(' or ')}</div>{/if}
</div>
{/if}

<style>
  .ok-line { align-self: flex-start; background: none; border: none; padding: 0; font: inherit; font-size: 12.5px; color: var(--ok); cursor: pointer; }
  .doctor { display: flex; flex-direction: column; gap: 2px; font-size: 12.5px; }
  .r { display: grid; grid-template-columns: 16px 130px 1fr; gap: 6px; }
  .ok span:first-child { color: var(--ok); }
  .warn span:first-child, .unknown span:first-child { color: var(--warn); }
  .fail span:first-child { color: var(--bad); }
  .info { color: var(--muted); }
  .note { margin-top: 4px; padding: 6px 10px; border-radius: 6px; background: color-mix(in srgb, var(--warn) 12%, var(--bg2)); }
</style>
