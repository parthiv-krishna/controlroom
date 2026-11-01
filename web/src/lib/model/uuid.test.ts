import { afterEach, describe, expect, it, vi } from 'vitest';
import { randomUuid } from './uuid';

const V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

describe('randomUuid', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('uses crypto.randomUUID when available', () => {
    expect(randomUuid()).toMatch(V4);
  });

  it('falls back to getRandomValues outside secure contexts', () => {
    const { getRandomValues } = crypto;
    vi.stubGlobal('crypto', { getRandomValues: getRandomValues.bind(crypto) });
    const ids = new Set(Array.from({ length: 100 }, randomUuid));
    expect(ids.size).toBe(100);
    for (const id of ids) expect(id).toMatch(V4);
  });
});
