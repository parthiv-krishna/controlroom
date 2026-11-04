// Small pure helpers: device names, deep links, prompt text and line diffs.
import type { ContentPart } from '../protocol';

/** A friendly device name from the user agent: "Firefox on Linux", "Safari on iPhone", … */
export function guessDeviceName(ua: string): string {
  const browser = /Edg(e|A|iOS)?\//.test(ua) ? 'Edge'
    : /Firefox\/|FxiOS\//.test(ua) ? 'Firefox'
    : /OPR\//.test(ua) ? 'Opera'
    : /Chrome\/|CriOS\/|Chromium\//.test(ua) ? 'Chrome'
    : /Safari\//.test(ua) ? 'Safari'
    : 'Browser';
  const os = /iPhone/.test(ua) ? 'iPhone'
    : /iPad/.test(ua) ? 'iPad'
    : /Android/.test(ua) ? 'Android'
    : /Windows/.test(ua) ? 'Windows'
    : /CrOS/.test(ua) ? 'ChromeOS'
    : /Macintosh|Mac OS X/.test(ua) ? 'macOS'
    : /Linux/.test(ua) ? 'Linux'
    : null;
  return os ? `${browser} on ${os}` : browser;
}

/** `#/s/<session>?n=<name>&m=<machine name>&st=<state>`: a push notification's link. */
export interface DeepLink {
  id: string;
  name: string | null;
  machine: string | null;
  state: string | null;
}

export function parseDeepLink(hash: string): DeepLink | null {
  const m = /^#\/s\/([^/?]+)(?:\?(.*))?$/.exec(hash);
  if (!m) return null;
  const q = new URLSearchParams(m[2] ?? '');
  try {
    return { id: decodeURIComponent(m[1]), name: q.get('n'), machine: q.get('m'), state: q.get('st') };
  } catch {
    return null;
  }
}

export const deepLinkHash = (id: string): string => `#/s/${encodeURIComponent(id)}`;

/** The text of a prompt (images and resources as placeholders). */
export function contentText(parts: readonly ContentPart[]): string {
  return parts
    .map((p) => (p.kind === 'text' ? p.text : p.kind === 'image' ? '[image]' : p.kind === 'resource' ? `[${p.name ?? p.uri}]` : ''))
    .join(' ')
    .trim();
}

export const textContent = (text: string): ContentPart[] => [{ kind: 'text', text }];

export interface DiffLine {
  kind: 'add' | 'del' | 'ctx';
  text: string;
}

/** A line diff (LCS) for request bodies; large inputs fall back to remove-all/add-all. */
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = before ? before.split('\n') : [];
  const b = after ? after.split('\n') : [];
  if (a.length * b.length > 250_000) return [...a.map((text) => ({ kind: 'del' as const, text })), ...b.map((text) => ({ kind: 'add' as const, text }))];
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: 'ctx', text: a[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) out.push({ kind: 'del', text: a[i++] });
    else out.push({ kind: 'add', text: b[j++] });
  }
  while (i < a.length) out.push({ kind: 'del', text: a[i++] });
  while (j < b.length) out.push({ kind: 'add', text: b[j++] });
  return out;
}
