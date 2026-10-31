<script lang="ts">
  // Red = failed or stopped, yellow = needs you (a hollow ring when only non-blocking questions),
  // green = finished and unread.
  import type { Health } from '../model/projects';

  let { h, row = false }: { h: Health; row?: boolean } = $props();
  const ring = $derived(!h.approvals && h.questions > 0);
  const yellow = $derived(ring ? h.questions : h.approvals);
</script>

<span class="sl" class:row title="{h.approvals} need you · {h.questions} questions · {h.problems} failed or stopped · {h.unread} finished">
  <span class="l red" class:on={h.problems}>{h.problems || ''}</span>
  <span class="l yellow" class:on={yellow} class:ring>{yellow || ''}</span>
  <span class="l green" class:on={h.unread}>{h.unread || ''}</span>
</span>

<style>
  .sl {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .sl.row {
    flex-direction: row;
    gap: 4px;
  }
  .l {
    width: 13px;
    height: 13px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 8.5px;
    font-weight: 800;
    line-height: 1;
    font-variant-numeric: tabular-nums;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--muted) 35%, transparent);
  }
  .row .l {
    width: 18px;
    height: 18px;
    font-size: 10.5px;
  }
  .l.on { box-shadow: none; }
  .red.on { background: color-mix(in srgb, var(--bad) 70%, #fff); color: #3d0a0a; }
  .yellow.on { background: var(--warn); color: #3a2a00; }
  .yellow.ring { background: none; color: var(--warn); box-shadow: inset 0 0 0 2px var(--warn); }
  .green.on { background: color-mix(in srgb, var(--ok) 80%, #fff); color: #062e1a; }
</style>
