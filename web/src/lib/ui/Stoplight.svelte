<script lang="ts">
  // One dot for the worst state, with its count: red = failed or stopped, yellow = needs you or an
  // open agent notice (a hollow ring when only non-blocking questions), green = finished and
  // unread; nothing when all is quiet. The tooltip lists every count.
  import type { Health } from '../model/projects';

  let { h, row = false }: { h: Health; row?: boolean } = $props();
  const worst = $derived.by((): { tone: string; n: number } | null => {
    if (h.problems) return { tone: 'red', n: h.problems };
    if (h.approvals || h.notices) return { tone: 'yellow', n: h.approvals + h.notices };
    if (h.questions) return { tone: 'yellow ring', n: h.questions };
    if (h.unread) return { tone: 'green', n: h.unread };
    return null;
  });
  const title = $derived(
    [
      h.approvals && `Need you: ${h.approvals}`,
      h.notices && `Agent notices: ${h.notices}`,
      h.questions && `Questions: ${h.questions}`,
      h.problems && `Failed or stopped: ${h.problems}`,
      h.unread && `Finished: ${h.unread}`,
    ]
      .filter(Boolean)
      .join('\n'),
  );
</script>

{#if worst}<span class="l {worst.tone}" class:row {title}>{worst.n}</span>{/if}

<style>
  .l {
    min-width: 15px;
    height: 15px;
    padding: 0 3px;
    border-radius: 999px;
    display: grid;
    place-items: center;
    font-size: 9px;
    font-weight: 800;
    line-height: 1;
    font-variant-numeric: tabular-nums;
  }
  .row { min-width: 18px; height: 18px; font-size: 10.5px; }
  .red { background: color-mix(in srgb, var(--bad) 70%, #fff); color: #3d0a0a; }
  .yellow { background: var(--warn); color: #3a2a00; }
  .yellow.ring { background: none; color: var(--warn); box-shadow: inset 0 0 0 2px var(--warn); }
  .green { background: color-mix(in srgb, var(--ok) 80%, #fff); color: #062e1a; }
</style>
