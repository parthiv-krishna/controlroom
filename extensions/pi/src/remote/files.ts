// `read`, `write` and `edit` on a workspace's machine (design §5), with pi's argument shapes
// and result texts. `edit` runs pi's own edit logic (exact then fuzzy matching, diff details)
// over `read_bytes` (`<id>/read`) and a compare-and-swap `write` (`<id>/write`), so a file
// changed meanwhile is a conflict, never a lost update. Images go through pi's read tool,
// which resizes them for the model.

import { createEditToolDefinition, createReadToolDefinition } from '@earendil-works/pi-coding-agent';

import type { SessionInfo } from '../protocol/SessionInfo.ts';
import { text, type Tool, type ToolCall, type ToolResult } from '../tool.ts';
import { MAX_BYTES, MAX_LINES, type Truncation, withNotes } from './format.ts';
import { hostCall, linkDropNote, type ResultOf } from './host.ts';
import { parseTarget, shown, type Target } from './target.ts';

/**
 * pi's tool logic resolves the `machine:path` strings it is given against this directory, which
 * only keys its per-file mutation queue: the operations never touch local files.
 */
const NOWHERE = '/';

/** The description of a `machine:path` parameter. */
export function pathParam(what: string): { type: 'string'; description: string } {
  return {
    type: 'string',
    description: `${what} as machine:path, e.g. devbox:~/src/api/README.md (~ is that machine's home; relative paths resolve against its workspace root)`,
  };
}

/** A file argument: a machine and a path are required. */
function fileTarget(info: SessionInfo, spec: string | undefined, tool: string): Target & { path: string } {
  const target = parseTarget(info, spec);
  if (target.path === null) throw new Error(`${tool} needs a file: machine:path, e.g. ${target.machine}:README.md.`);
  return target as Target & { path: string };
}

/** `read` params. */
export interface ReadParams {
  path: string;
  offset?: number;
  limit?: number;
}

/** `read` details (pi's renderer reads `truncation`). */
export type ReadDetails = { truncation?: Truncation } | undefined;

export const read: Tool<ReadParams, ReadDetails> = {
  name: 'read',
  label: 'read',
  description:
    'Read a file on a machine. Supports text files and images (jpg, png, gif, webp, bmp); images are sent as attachments. ' +
    `Text output is truncated to ${MAX_LINES} lines or ${MAX_BYTES / 1024}KB (whichever is hit first). Use offset/limit for large files; ` +
    'when you need the full file, continue with offset until complete.',
  promptSnippet: 'Read file contents (machine:path)',
  promptGuidelines: ['Use read to examine files instead of cat or sed.'],
  parameters: {
    type: 'object',
    properties: {
      path: pathParam('The file'),
      offset: { type: 'number', description: 'Line number to start reading from (1-indexed)' },
      limit: { type: 'number', description: 'Maximum number of lines to read' },
    },
    required: ['path'],
  },
  async execute(call, params) {
    const info = await call.client.sessionInfo();
    const target = fileTarget(info, params.path, 'read');
    const { result, linkDrops } = await hostCall(call, info, target.machine, {
      op: 'read',
      path: target.path,
      ...(params.offset !== undefined ? { offset: params.offset } : {}),
      ...(params.limit !== undefined ? { limit: params.limit } : {}),
    });
    const where = shown(info, target.machine, result.path);
    const content = result.content;
    let out: ToolResult<ReadDetails>;
    if (content.kind === 'text') out = readText(where, result.path, content, params);
    else if (content.kind === 'image') out = await readImage(call, info, target.machine, where, result, content.mime_type);
    else throw new Error(`${where}: the host returned content this extension can't show.`);
    return withNotes(out, linkDropNote(target.machine, linkDrops));
  },
};

type TextContent = Extract<ResultOf<'read'>['content'], { kind: 'text' }>;

/** A text window as pi's read shows it, with its continuation notice. */
function readText(where: string, path: string, c: TextContent, params: ReadParams): ToolResult<ReadDetails> {
  const start = c.start_line;
  const truncation = (by: Truncation['truncatedBy'], firstLine = false): Truncation => ({
    truncated: true,
    truncatedBy: by,
    totalLines: c.total_lines,
    outputLines: c.lines.length,
    firstLineExceedsLimit: firstLine,
    maxLines: MAX_LINES,
    maxBytes: MAX_BYTES,
  });
  if (c.first_line_too_long) {
    const hint = `[Line ${start} of ${where} exceeds the ${MAX_BYTES / 1024}KB limit. Use bash: sed -n '${start}p' ${path} | head -c ${MAX_BYTES}]`;
    return { content: text(hint), details: { truncation: truncation('bytes', true) } };
  }
  if (!c.lines.length && start > c.total_lines) {
    throw new Error(`Offset ${params.offset ?? start} is beyond end of file (${c.total_lines} lines total)`);
  }
  const end = start + c.lines.length - 1;
  const body = c.lines.join('\n');
  if (c.cut) {
    const limit = c.cut === 'bytes' ? ` (${MAX_BYTES / 1024}KB limit)` : '';
    const notice = `[Showing lines ${start}-${end} of ${c.total_lines}${limit}. Use offset=${end + 1} to continue.]`;
    return { content: text(`${body}\n\n${notice}`), details: { truncation: truncation(c.cut === 'bytes' ? 'bytes' : 'lines') } };
  }
  if (params.limit !== undefined && end < c.total_lines) {
    return { content: text(`${body}\n\n[${c.total_lines - end} more lines in file. Use offset=${end + 1} to continue.]`), details: undefined };
  }
  return { content: text(body), details: undefined };
}

/** An image: its bytes (`<id>/bytes`) handed to pi's read tool, which resizes it. */
async function readImage(
  call: ToolCall,
  info: SessionInfo,
  machine: string,
  where: string,
  found: ResultOf<'read'>,
  mimeType: string,
): Promise<ToolResult<ReadDetails>> {
  const { result } = await hostCall(call, info, machine, { op: 'read_bytes', path: found.path }, { id: `${call.id}/bytes` });
  if (result.length < result.size) throw new Error(`${where} is too large to attach (${result.size} bytes).`);
  const bytes = Buffer.from(result.data, 'base64');
  const piRead = createReadToolDefinition(NOWHERE, {
    operations: { access: async () => {}, readFile: async () => bytes, detectImageMimeType: async () => mimeType },
  });
  return (await piRead.execute(call.id, { path: where }, call.signal, undefined, call.ctx as never)) as ToolResult<ReadDetails>;
}

/** `write` params. */
export interface WriteParams {
  path: string;
  content: string;
}

export const write: Tool<WriteParams, undefined> = {
  name: 'write',
  label: 'write',
  description:
    'Write content to a file on a machine. Creates the file if it does not exist, overwrites it if it does. Automatically creates parent directories.',
  promptSnippet: 'Create or overwrite files (machine:path)',
  promptGuidelines: ['Use write only for new files or complete rewrites.'],
  parameters: {
    type: 'object',
    properties: { path: pathParam('The file'), content: { type: 'string', description: 'Content to write to the file' } },
    required: ['path', 'content'],
  },
  async execute(call, params) {
    const info = await call.client.sessionInfo();
    const target = fileTarget(info, params.path, 'write');
    const { result, linkDrops } = await hostCall(call, info, target.machine, { op: 'write', path: target.path, content: params.content });
    const said = `Successfully wrote ${result.bytes} bytes to ${shown(info, target.machine, result.path)}`;
    return withNotes({ content: text(said), details: undefined }, linkDropNote(target.machine, linkDrops));
  },
};

/** `edit` params (pi's shape). */
export interface EditParams {
  path: string;
  edits: { oldText: string; newText: string }[];
}

/** `edit` details: pi's diff for its renderer and the timeline. */
export type EditDetails = { diff: string; patch: string; firstChangedLine?: number } | undefined;

const piEdit = createEditToolDefinition(NOWHERE);

export const edit: Tool<EditParams, EditDetails> = {
  name: 'edit',
  label: 'edit',
  description: piEdit.description,
  promptSnippet: 'Make precise file edits with exact text replacement, including multiple disjoint edits in one call (machine:path)',
  promptGuidelines: piEdit.promptGuidelines,
  parameters: {
    type: 'object',
    properties: {
      path: pathParam('The file to edit'),
      edits: {
        type: 'array',
        description:
          'One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.',
        items: {
          type: 'object',
          properties: {
            oldText: {
              type: 'string',
              description:
                'Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.',
            },
            newText: { type: 'string', description: 'Replacement text for this targeted edit.' },
          },
          required: ['oldText', 'newText'],
        },
      },
    },
    required: ['path', 'edits'],
  },
  prepareArguments: piEdit.prepareArguments as (args: unknown) => EditParams,
  async execute(call, params) {
    const info = await call.client.sessionInfo();
    const target = fileTarget(info, params.path, 'edit');
    let drops = 0;
    let base: { path: string; sha256: string } | null = null;
    const operations = {
      access: async () => {},
      readFile: async () => {
        const { result, linkDrops } = await hostCall(call, info, target.machine, { op: 'read_bytes', path: target.path }, { id: `${call.id}/read` });
        drops += linkDrops;
        if (result.sha256 === null) throw new Error(`${target.display} is too large to edit (${result.size} bytes); use write or bash.`);
        base = { path: result.path, sha256: result.sha256 };
        return Buffer.from(result.data, 'base64');
      },
      writeFile: async (_path: string, content: string) => {
        if (!base) throw new Error(`${target.display} was not read before writing.`);
        const step = { op: 'write' as const, path: base.path, content, expect_sha256: base.sha256 };
        drops += (await hostCall(call, info, target.machine, step, { id: `${call.id}/write` })).linkDrops;
      },
    };
    const remoteEdit = createEditToolDefinition(NOWHERE, { operations });
    const result = await remoteEdit.execute(call.id, { path: target.display, edits: params.edits }, call.signal, undefined, call.ctx as never);
    return withNotes(result as ToolResult<EditDetails>, linkDropNote(target.machine, drops));
  },
};
