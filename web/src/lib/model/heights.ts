// Windowing math for the timeline: row heights in a Fenwick tree, so one row changing height (a
// streamed message growing) costs O(log n), and the row at a scroll offset is found in O(log n).

/** Row heights with prefix sums. Unmeasured rows use an estimate. */
export class HeightIndex {
  private readonly heights: Float64Array;
  private readonly tree: Float64Array;

  constructor(heights: readonly number[]) {
    const n = heights.length;
    this.heights = Float64Array.from(heights);
    this.tree = new Float64Array(n + 1);
    // O(n) build: each node passes its sum on to its parent.
    for (let i = 1; i <= n; i++) {
      this.tree[i] += heights[i - 1];
      const parent = i + (i & -i);
      if (parent <= n) this.tree[parent] += this.tree[i];
    }
  }

  get length(): number {
    return this.heights.length;
  }

  height(i: number): number {
    return this.heights[i];
  }

  /** Sets row `i`'s height; returns whether it changed. */
  set(i: number, h: number): boolean {
    const delta = h - this.heights[i];
    if (delta === 0) return false;
    this.heights[i] = h;
    for (let j = i + 1; j < this.tree.length; j += j & -j) this.tree[j] += delta;
    return true;
  }

  /** The top of row `i` (the sum of the rows before it); `offset(length)` is the total. */
  offset(i: number): number {
    let sum = 0;
    for (let j = Math.min(i, this.length); j > 0; j -= j & -j) sum += this.tree[j];
    return sum;
  }

  total(): number {
    return this.offset(this.length);
  }

  /** The row containing offset `y` (clamped to the first and last rows; 0 when empty). */
  indexAt(y: number): number {
    const n = this.length;
    if (n === 0 || y <= 0) return 0;
    let pos = 0;
    let rest = y;
    for (let step = 1 << Math.floor(Math.log2(n)); step > 0; step >>= 1) {
      const next = pos + step;
      if (next <= n && this.tree[next] <= rest) {
        pos = next;
        rest -= this.tree[next];
      }
    }
    return Math.min(pos, n - 1);
  }
}

/** The rows to render: `[start, end)`, covering the viewport plus `overscan` pixels each way. */
export function visibleRange(index: HeightIndex, scrollTop: number, viewport: number, overscan: number): { start: number; end: number } {
  if (index.length === 0) return { start: 0, end: 0 };
  const start = index.indexAt(scrollTop - overscan);
  const end = Math.min(index.length, index.indexAt(scrollTop + viewport + overscan) + 1);
  return { start, end };
}
