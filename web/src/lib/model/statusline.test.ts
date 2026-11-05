import { describe, expect, it } from 'vitest';
import type { ContextUsage, SessionUsage } from '../protocol';
import { formatTokens, partsToDrop, statusParts, type PartKey } from './statusline';

const usage = (input: number, output: number, cache_read: number, cache_write: number, cost: number, latest_cache_hit: number | null): SessionUsage => ({
  tokens: { input, output, cache_read, cache_write },
  cost,
  latest_cache_hit,
});
const ctx = (used: number, size: number | null): ContextUsage => ({ used, size, cost: null });
const line = (u: SessionUsage, c: ContextUsage | null) => statusParts(u, c).map((p) => p.text).join(' ');
const ZERO = usage(0, 0, 0, 0, 0, null);

describe('formatTokens', () => {
  it('matches pi: as is, one decimal k, rounded k, one decimal M, rounded M', () => {
    expect([0, 999, 1000, 1234, 9999, 10_000, 430_000, 999_499, 1_000_000, 9_949_999, 10_000_000, 303_000_000].map(formatTokens)).toEqual([
      '0', '999', '1.0k', '1.2k', '10.0k', '10k', '430k', '999k', '1.0M', '9.9M', '10M', '303M',
    ]);
  });
});

describe('statusParts', () => {
  it("renders pi's footer example", () => {
    expect(line(usage(430_000, 378_000, 303_000_000, 54_000_000, 337.7, 0.993), ctx(160_000, 1_000_000))).toBe('↑430k ↓378k R303M W54M CH99.3% $337.700 16.0%/1.0M');
  });

  it('labels every part with its exact value', () => {
    const parts = statusParts(usage(430_000, 378_000, 303_000_000, 54_000_000, 337.7, 0.993), ctx(160_000, 1_000_000));
    expect(parts.map((p) => `${p.label}: ${p.value}`)).toEqual([
      'input: 430,000',
      'output: 378,000',
      'cache read: 303,000,000',
      'cache write: 54,000,000',
      'cache hit (latest reply): 99.3%',
      'cost (USD): $337.700',
      'context (used / window): 160,000 / 1,000,000',
    ]);
  });

  it('omits zero parts, and the cache hit without cache use or a latest rate', () => {
    expect(line(usage(1200, 0, 0, 0, 0, 0), ctx(1200, 200_000))).toBe('↑1.2k 0.6%/200k');
    expect(line(usage(0, 5, 0, 0, 0.01, 0.5), ctx(5, 200_000))).toBe('↓5 $0.010 0.0%/200k');
    expect(line(usage(10, 5, 90, 0, 0, null), ctx(105, 1000))).toBe('↑10 ↓5 R90 10.5%/1.0k');
    expect(line(usage(10, 5, 0, 20, 0, 0), ctx(35, 1000))).toBe('↑10 ↓5 W20 CH0.0% 3.5%/1.0k');
  });

  it('shows an unknown context as ?, and nothing before any report', () => {
    expect(line(usage(10, 5, 0, 0, 0, 0), null)).toBe('↑10 ↓5 ?');
    expect(line(usage(10, 5, 0, 0, 0, 0), ctx(15, null))).toBe('↑10 ↓5 ?');
    expect(line(ZERO, ctx(15, 1000))).toBe('1.5%/1.0k');
    expect(statusParts(ZERO, null)).toEqual([]);
  });

  it('warns above 70 % and errs above 90 %', () => {
    const level = (used: number) => statusParts(ZERO, ctx(used, 1000)).at(-1)!.level;
    expect([700, 701, 900, 901, 1200].map(level)).toEqual([null, 'warn', 'warn', 'full', 'full']);
  });
});

describe('partsToDrop', () => {
  const parts = statusParts(usage(430_000, 378_000, 303_000_000, 54_000_000, 337.7, 0.993), ctx(160_000, 1_000_000));
  // One pixel per character, a 1px gap, no padding.
  const widths = new Map(parts.map((p) => [p.key, p.text.length] as [PartKey, number]));
  const drop = (available: number) => [...partsToDrop(parts, widths, 1, 0, available)];

  it('keeps everything that fits', () => {
    expect(drop(50)).toEqual([]);
  });

  it('drops W, then R, then CH, …, never the context', () => {
    expect(drop(49)).toEqual(['cache_write']);
    expect(drop(45)).toEqual(['cache_write']);
    expect(drop(44)).toEqual(['cache_write', 'cache_read']);
    expect(drop(39)).toEqual(['cache_write', 'cache_read']);
    expect(drop(38)).toEqual(['cache_write', 'cache_read', 'cache_hit']);
    expect(drop(1)).toEqual(['cache_write', 'cache_read', 'cache_hit', 'input', 'output', 'cost']);
  });

  it('skips parts the line does not have', () => {
    const few = statusParts(usage(10, 5, 0, 0, 0.5, null), ctx(15, 1000));
    const w = new Map(few.map((p) => [p.key, p.text.length] as [PartKey, number]));
    expect([...partsToDrop(few, w, 1, 0, 19)]).toEqual(['input']);
  });
});
