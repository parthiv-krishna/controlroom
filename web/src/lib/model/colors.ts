// The color swatches offered for projects and profiles (any CSS color is accepted).

export const SWATCHES = ['#f97316', '#facc15', '#34d399', '#22d3ee', '#60a5fa', '#a78bfa', '#f472b6', '#f43f5e'];

/** The first swatch none of `used` has, else the next in turn. */
export function nextSwatch(used: readonly string[]): string {
  return SWATCHES.find((c) => !used.includes(c)) ?? SWATCHES[used.length % SWATCHES.length];
}
