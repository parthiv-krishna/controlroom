// The session statusline: the session's usage in pi's footer format (pi 1.2.0,
// modes/interactive/components/footer.js), e.g. `↑430k ↓378k R303M W54M CH99.3% $337.700 16.0%/1.0M`,
// and which parts to drop when it doesn't fit.
import type { ContextUsage, SessionUsage } from '../protocol';

/** One part of the statusline. */
export type PartKey = 'input' | 'output' | 'cache_read' | 'cache_write' | 'cache_hit' | 'cost' | 'context';

export interface StatusPart {
  key: PartKey;
  /** As pi shows it, e.g. `R303M`. */
  text: string;
  /** What it is, e.g. `cache read`. */
  label: string;
  /** The exact value, e.g. `303,000,000`. */
  value: string;
  /** Context fill: `warn` above 70 %, `full` above 90 %. */
  level: 'warn' | 'full' | null;
}

/** Parts dropped first when the line doesn't fit; the context part always stays. */
export const DROP_ORDER: readonly PartKey[] = ['cache_write', 'cache_read', 'cache_hit', 'input', 'output', 'cost'];

/** A token count as pi's footer shows it: `950`, `4.2k`, `430k`, `1.0M`, `303M`. */
export function formatTokens(count: number): string {
  if (count < 1000) return count.toString();
  if (count < 10_000) return `${(count / 1000).toFixed(1)}k`;
  if (count < 1_000_000) return `${Math.round(count / 1000)}k`;
  if (count < 10_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  return `${Math.round(count / 1_000_000)}M`;
}

const exact = (n: number): string => n.toLocaleString('en-US');

/**
 * The statusline's parts in pi's order. Like pi, token and cost parts appear only when non-zero,
 * the cache hit rate only when the session read or wrote the cache, and the context part always
 * (`?` while its size or fill is unknown). Empty while nothing was reported.
 */
export function statusParts(usage: SessionUsage, context: ContextUsage | null): StatusPart[] {
  const t = usage.tokens;
  const parts: StatusPart[] = [];
  const add = (key: PartKey, text: string, label: string, value: string, level: StatusPart['level'] = null) => parts.push({ key, text, label, value, level });
  if (t.input) add('input', `↑${formatTokens(t.input)}`, 'input', exact(t.input));
  if (t.output) add('output', `↓${formatTokens(t.output)}`, 'output', exact(t.output));
  if (t.cache_read) add('cache_read', `R${formatTokens(t.cache_read)}`, 'cache read', exact(t.cache_read));
  if (t.cache_write) add('cache_write', `W${formatTokens(t.cache_write)}`, 'cache write', exact(t.cache_write));
  if ((t.cache_read || t.cache_write) && usage.latest_cache_hit !== null) {
    const pct = (usage.latest_cache_hit * 100).toFixed(1);
    add('cache_hit', `CH${pct}%`, 'cache hit (latest reply)', `${pct}%`);
  }
  if (usage.cost) add('cost', `$${usage.cost.toFixed(3)}`, 'cost (USD)', `$${usage.cost.toFixed(3)}`);
  if (parts.length || context) parts.push(contextPart(context));
  return parts;
}

/** pi's `percent%/window`, colored by fill; `?` while the size or the fill is unknown. */
function contextPart(context: ContextUsage | null): StatusPart {
  const size = context?.size ?? null;
  const window = size !== null ? `/${formatTokens(size)}` : '';
  if (!context || !size) {
    const value = context ? `${exact(context.used)} / unknown` : 'not reported yet';
    return { key: 'context', text: `?${window}`, label: 'context (used / window)', value, level: null };
  }
  const pct = (context.used / size) * 100;
  return {
    key: 'context',
    text: `${pct.toFixed(1)}%${window}`,
    label: 'context (used / window)',
    value: `${exact(context.used)} / ${exact(size)}`,
    level: pct > 90 ? 'full' : pct > 70 ? 'warn' : null,
  };
}

/**
 * The parts to hide so the line fits in `available` pixels: drops parts in DROP_ORDER until the
 * shown parts' `widths` (by key) plus a `gap` between neighbors and `extra` (padding) fit.
 */
export function partsToDrop(parts: readonly StatusPart[], widths: ReadonlyMap<PartKey, number>, gap: number, extra: number, available: number): Set<PartKey> {
  const dropped = new Set<PartKey>();
  const width = () => {
    const shown = parts.filter((p) => !dropped.has(p.key));
    return extra + shown.reduce((sum, p) => sum + (widths.get(p.key) ?? 0), 0) + gap * Math.max(0, shown.length - 1);
  };
  for (const key of DROP_ORDER) {
    if (width() <= available) break;
    if (parts.some((p) => p.key === key)) dropped.add(key);
  }
  return dropped;
}
