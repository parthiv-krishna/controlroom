import { describe, expect, it } from 'vitest';
import { HeightIndex, visibleRange } from './heights';

describe('HeightIndex', () => {
  it('sums offsets and finds the row at an offset', () => {
    const idx = new HeightIndex([10, 20, 30, 40]);
    expect([0, 1, 2, 3, 4].map((i) => idx.offset(i))).toEqual([0, 10, 30, 60, 100]);
    expect(idx.total()).toBe(100);
    expect([-5, 0, 9, 10, 29, 30, 99, 100, 500].map((y) => idx.indexAt(y))).toEqual([0, 0, 0, 1, 1, 2, 3, 3, 3]);
  });

  it('updates one row in place', () => {
    const idx = new HeightIndex([10, 10, 10]);
    expect(idx.set(1, 50)).toBe(true);
    expect(idx.set(1, 50)).toBe(false);
    expect(idx.offset(2)).toBe(60);
    expect(idx.total()).toBe(70);
    expect(idx.indexAt(59)).toBe(1);
    expect(idx.height(1)).toBe(50);
  });

  it('agrees with a naive prefix sum on random data', () => {
    let seed = 7;
    const rand = () => (seed = (seed * 16807) % 2147483647) / 2147483647;
    const hs = Array.from({ length: 333 }, () => Math.round(rand() * 200) + 1);
    const idx = new HeightIndex(hs);
    for (let k = 0; k < 200; k++) {
      const i = Math.floor(rand() * hs.length);
      hs[i] = Math.round(rand() * 300) + 1;
      idx.set(i, hs[i]);
    }
    let acc = 0;
    hs.forEach((h, i) => {
      expect(idx.offset(i)).toBe(acc);
      expect(idx.indexAt(acc)).toBe(i);
      expect(idx.indexAt(acc + h - 0.5)).toBe(i);
      acc += h;
    });
  });

  it('handles an empty list', () => {
    const idx = new HeightIndex([]);
    expect(idx.total()).toBe(0);
    expect(idx.indexAt(100)).toBe(0);
    expect(visibleRange(idx, 0, 500, 100)).toEqual({ start: 0, end: 0 });
  });
});

describe('visibleRange', () => {
  const idx = new HeightIndex(Array.from({ length: 1000 }, () => 50));

  it('covers the viewport plus the overscan', () => {
    expect(visibleRange(idx, 0, 500, 100)).toEqual({ start: 0, end: 13 });
    expect(visibleRange(idx, 10_000, 500, 100)).toEqual({ start: 198, end: 213 });
  });

  it('clamps at the end', () => {
    expect(visibleRange(idx, 49_800, 500, 100)).toEqual({ start: 994, end: 1000 });
  });
});
