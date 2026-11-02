// Number formatting for compact displays.

/** A token count as `950`, `12k` or `1.2M`. */
export const compact = (n: number): string => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : String(n));

/** A short duration with sub-second precision: `12 ms`, `1.4 s`, `3m`. */
export const elapsed = (ms: number): string => (ms < 1000 ? `${Math.round(ms)} ms` : ms < 60_000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms / 60_000)}m`);
