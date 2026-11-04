// `grep`, `find` and `ls` on a workspace's machine (design §5), with pi's argument shapes,
// limits and notices. grep and find say where they searched (`[in devbox:~/src/api]`), since
// their output is relative to it.

import type { SessionInfo } from '../protocol/SessionInfo.ts';
import { text, type Tool, type ToolCall, type ToolResult } from '../tool.ts';
import { capBytes, MAX_BYTES, type Truncation, withNotes } from './format.ts';
import { pathParam } from './files.ts';
import { hostCall, linkDropNote } from './host.ts';
import { parseTarget, shown } from './target.ts';

const GREP_LIMIT = 100;
const FIND_LIMIT = 1000;
const LS_LIMIT = 500;
const LINE_CHARS = 500;

/** A capped output with pi's notices, and the details its renderers read. */
function finish<D extends Record<string, unknown>>(
  body: string,
  notices: string[],
  details: D,
  machine: string,
  linkDrops: number,
): ToolResult<(D & { truncation?: Partial<Truncation> }) | undefined> {
  const cut = capBytes(body);
  const all = [...notices];
  const out: D & { truncation?: Partial<Truncation> } = { ...details };
  if (cut) {
    all.push(`${MAX_BYTES / 1024}KB limit reached`);
    out.truncation = { truncated: true, truncatedBy: 'bytes', outputLines: cut.lines, maxBytes: MAX_BYTES };
  }
  const said = all.length ? `${cut?.text ?? body}\n\n[${all.join('. ')}]` : (cut?.text ?? body);
  const result = { content: text(said), details: Object.keys(out).length ? out : undefined };
  return withNotes(result, linkDropNote(machine, linkDrops));
}

async function where(call: ToolCall, path: string | undefined): Promise<{ info: SessionInfo; machine: string; path: string | null }> {
  const info = await call.client.sessionInfo();
  const target = parseTarget(info, path);
  return { info, machine: target.machine, path: target.path };
}

/** `grep` params (pi's shape). */
export interface GrepParams {
  pattern: string;
  path?: string;
  glob?: string;
  ignoreCase?: boolean;
  literal?: boolean;
  context?: number;
  limit?: number;
}

/** `grep` details (pi's renderer reads them). */
export type GrepDetails = { matchLimitReached?: number; linesTruncated?: boolean; truncation?: Partial<Truncation> } | undefined;

export const grep: Tool<GrepParams, GrepDetails> = {
  name: 'grep',
  label: 'grep',
  description:
    'Search file contents on a machine for a pattern. Returns matching lines with file paths (relative to the searched directory) and line numbers. ' +
    `Respects .gitignore. Output is truncated to ${GREP_LIMIT} matches or ${MAX_BYTES / 1024}KB (whichever is hit first). Long lines are truncated to ${LINE_CHARS} chars.`,
  promptSnippet: 'Search file contents for patterns (respects .gitignore; machine:path)',
  parameters: {
    type: 'object',
    properties: {
      pattern: { type: 'string', description: 'Search pattern (regex or literal string)' },
      path: pathParam('Directory or file to search (default: the workspace, when the session has one)'),
      glob: { type: 'string', description: "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
      ignoreCase: { type: 'boolean', description: 'Case-insensitive search (default: false)' },
      literal: { type: 'boolean', description: 'Treat pattern as literal string instead of regex (default: false)' },
      context: { type: 'number', description: 'Number of lines to show before and after each match (default: 0)' },
      limit: { type: 'number', description: `Maximum number of matches to return (default: ${GREP_LIMIT})` },
    },
    required: ['pattern'],
  },
  async execute(call, params) {
    const at = await where(call, params.path);
    const limit = Math.max(1, params.limit ?? GREP_LIMIT);
    const { result, linkDrops } = await hostCall(call, at.info, at.machine, {
      op: 'grep',
      pattern: params.pattern,
      path: at.path,
      glob: params.glob ?? null,
      ignore_case: params.ignoreCase ?? false,
      literal: params.literal ?? false,
      context: params.context ?? null,
      limit,
    });
    const header = `[in ${shown(at.info, at.machine, result.path)}]`;
    if (!result.lines.length) return finish(`${header}\nNo matches found`, [], {}, at.machine, linkDrops);
    const lines = result.lines.map((l) => (l.context ? `${l.path}-${l.line}- ${l.text}` : `${l.path}:${l.line}: ${l.text}`));
    const notices: string[] = [];
    const details: NonNullable<GrepDetails> = {};
    if (result.limit_reached) {
      notices.push(`${limit} matches limit reached. Use limit=${limit * 2} for more, or refine pattern`);
      details.matchLimitReached = limit;
    }
    if (result.lines_truncated) {
      notices.push(`Some lines truncated to ${LINE_CHARS} chars. Use read to see full lines`);
      details.linesTruncated = true;
    }
    return finish(`${header}\n${lines.join('\n')}`, notices, details, at.machine, linkDrops);
  },
};

/** `find` params (pi's shape). */
export interface FindParams {
  pattern: string;
  path?: string;
  limit?: number;
}

/** `find` details (pi's renderer reads them). */
export type FindDetails = { resultLimitReached?: number; truncation?: Partial<Truncation> } | undefined;

export const find: Tool<FindParams, FindDetails> = {
  name: 'find',
  label: 'find',
  description:
    'Search for files by glob pattern on a machine. Returns matching file paths relative to the search directory. Respects .gitignore. ' +
    `Output is truncated to ${FIND_LIMIT} results or ${MAX_BYTES / 1024}KB (whichever is hit first).`,
  promptSnippet: 'Find files by glob pattern (respects .gitignore; machine:path)',
  parameters: {
    type: 'object',
    properties: {
      pattern: { type: 'string', description: "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'" },
      path: pathParam('Directory to search in (default: the workspace, when the session has one)'),
      limit: { type: 'number', description: `Maximum number of results (default: ${FIND_LIMIT})` },
    },
    required: ['pattern'],
  },
  async execute(call, params) {
    const at = await where(call, params.path);
    const limit = Math.max(1, params.limit ?? FIND_LIMIT);
    const { result, linkDrops } = await hostCall(call, at.info, at.machine, { op: 'find', pattern: params.pattern, path: at.path, limit });
    const header = `[in ${shown(at.info, at.machine, result.path)}]`;
    if (!result.paths.length) return finish(`${header}\nNo files found matching pattern`, [], {}, at.machine, linkDrops);
    const notices: string[] = [];
    const details: NonNullable<FindDetails> = {};
    if (result.limit_reached) {
      notices.push(`${limit} results limit reached. Use limit=${limit * 2} for more, or refine pattern`);
      details.resultLimitReached = limit;
    }
    return finish(`${header}\n${result.paths.join('\n')}`, notices, details, at.machine, linkDrops);
  },
};

/** `ls` params (pi's shape). */
export interface LsParams {
  path?: string;
  limit?: number;
}

/** `ls` details (pi's renderer reads them). */
export type LsDetails = { entryLimitReached?: number; truncation?: Partial<Truncation> } | undefined;

export const ls: Tool<LsParams, LsDetails> = {
  name: 'ls',
  label: 'ls',
  description:
    "List a directory on a machine. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. " +
    `Output is truncated to ${LS_LIMIT} entries or ${MAX_BYTES / 1024}KB (whichever is hit first).`,
  promptSnippet: 'List directory contents (machine:path)',
  parameters: {
    type: 'object',
    properties: {
      path: pathParam('Directory to list (default: the workspace, when the session has one)'),
      limit: { type: 'number', description: `Maximum number of entries to return (default: ${LS_LIMIT})` },
    },
  },
  async execute(call, params) {
    const at = await where(call, params.path);
    const limit = Math.max(1, params.limit ?? LS_LIMIT);
    const { result, linkDrops } = await hostCall(call, at.info, at.machine, { op: 'ls', path: at.path, limit });
    if (!result.entries.length) return finish('(empty directory)', [], {}, at.machine, linkDrops);
    const names = result.entries.map((e) => (e.kind === 'dir' ? `${e.name}/` : e.name));
    const notices: string[] = [];
    const details: NonNullable<LsDetails> = {};
    if (result.total > result.entries.length) {
      notices.push(`${limit} entries limit reached (${result.total} in total). Use limit=${limit * 2} for more`);
      details.entryLimitReached = limit;
    }
    return finish(names.join('\n'), notices, details, at.machine, linkDrops);
  },
};
