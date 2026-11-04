// Output limits and result helpers shared by the remote tools, matching pi's built-ins (2000
// lines or 50 KB of text per result), so the model sees the notices it knows.

import type { Content, ToolResult } from '../tool.ts';

/** Lines per text result. */
export const MAX_LINES = 2000;
/** Bytes per text result. */
export const MAX_BYTES = 50 * 1024;

/** The parts of pi's `TruncationResult` its renderers read. */
export interface Truncation {
  truncated: boolean;
  truncatedBy: 'lines' | 'bytes' | null;
  totalLines: number;
  outputLines: number;
  firstLineExceedsLimit: boolean;
  maxLines: number;
  maxBytes: number;
}

/** A cut of `text` to whole lines within `MAX_BYTES`; `null` when it fits. */
export function capBytes(text: string): { text: string; lines: number } | null {
  if (Buffer.byteLength(text, 'utf8') <= MAX_BYTES) return null;
  const kept: string[] = [];
  let bytes = 0;
  for (const line of text.split('\n')) {
    const size = Buffer.byteLength(line, 'utf8') + 1;
    if (bytes + size > MAX_BYTES) break;
    kept.push(line);
    bytes += size;
  }
  return { text: kept.join('\n'), lines: kept.length };
}

/** Appends notes (non-empty ones) to a result's first text block. */
export function withNotes<D>(result: ToolResult<D>, ...notes: string[]): ToolResult<D> {
  const extra = notes.filter(Boolean);
  if (!extra.length) return result;
  const index = result.content.findIndex((c) => c.type === 'text');
  const content: Content[] = [...result.content];
  if (index < 0) content.unshift({ type: 'text', text: extra.join('\n') });
  else {
    const block = content[index] as { type: 'text'; text: string };
    content[index] = { type: 'text', text: `${block.text}\n\n${extra.join('\n')}` };
  }
  return { ...result, content };
}
