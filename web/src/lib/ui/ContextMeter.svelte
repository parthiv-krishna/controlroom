<script lang="ts" module>
  /** Context fill: yellow from 70 %, red from 90 %. */
  export const ctxLevel = (used: number, size: number | null): 'ok' | 'warn' | 'full' => {
    const f = size ? used / size : 0;
    return f >= 0.9 ? 'full' : f >= 0.7 ? 'warn' : 'ok';
  };
</script>

<script lang="ts">
  import type { ContextUsage } from '../protocol';

  let { usage, compact = false }: { usage: ContextUsage; compact?: boolean } = $props();
  const pct = $derived(usage.size ? Math.min(100, (usage.used / usage.size) * 100) : 0);
  const level = $derived(ctxLevel(usage.used, usage.size));
  const k = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : String(n));
  const title = $derived(
    `Context ${usage.used.toLocaleString()}${usage.size ? ` / ${usage.size.toLocaleString()}` : ''} tokens${usage.cost != null ? ` · $${usage.cost.toFixed(2)}` : ''}`,
  );
</script>

<span class="ctx {level}" class:compact {title} role="meter" aria-label="Context window" aria-valuemin={0} aria-valuemax={usage.size ?? usage.used} aria-valuenow={usage.used}>
  <span class="bar"><span class="fill" style:width="{pct}%"></span></span>
  {#if !compact}<span class="n">{k(usage.used)}{usage.size ? `/${k(usage.size)}` : ''}</span>{/if}
</span>

<style>
  .ctx { display: inline-flex; align-items: center; gap: 6px; font-size: 11.5px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .bar { width: 64px; height: 5px; border-radius: 3px; background: var(--bg3); overflow: hidden; }
  .fill { display: block; height: 100%; border-radius: 3px; background: var(--ok); transition: width 0.3s ease; }
  .warn .fill { background: var(--warn); }
  .warn .n { color: var(--warn); }
  .full .fill { background: var(--bad); }
  .full .n { color: var(--bad); }
  .compact .bar { width: 28px; height: 4px; }
</style>
