// What every tool of the extension is: a plain definition (name, description, JSON Schema
// parameters) plus an `execute` that talks to the agent daemon, so tools run and test without
// pi's runtime. Results carry model-facing content and `details` for rendering.

import type { AgentClient } from './client.ts';

/** Model-facing content of a result. */
export type Content = { type: 'text'; text: string } | { type: 'image'; data: string; mimeType: string };

/** What a tool returns to pi. */
export interface ToolResult<D> {
  content: Content[];
  details: D;
  /** A failed call that still carries its output (pi shows it as an error). */
  isError?: boolean;
}

/** One call of a tool. */
export interface ToolCall {
  /** The session's agent daemon. */
  client: AgentClient;
  /** pi's tool call id: the idempotency key of everything the call does. */
  id: string;
  /** pi's abort signal (the user or the runtime cancelled the run). */
  signal?: AbortSignal;
  /** Streams a partial result into pi's tool updates. */
  onUpdate?: (partial: ToolResult<unknown>) => void;
  /** pi's tool context, passed through to pi's own tool logic (edit, image reads). */
  ctx?: unknown;
}

/** A tool independent of pi's types. */
export interface Tool<P, D> {
  name: string;
  label: string;
  description: string;
  promptSnippet: string;
  promptGuidelines?: string[];
  parameters: object;
  /** Normalizes raw arguments before validation (pi's edit shim). */
  prepareArguments?: (args: unknown) => P;
  execute(call: ToolCall, params: P): Promise<ToolResult<D>>;
}

/** A single text block. */
export const text = (s: string): Content[] => [{ type: 'text', text: s }];
