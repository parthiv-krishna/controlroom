// Wires the extension into pi: the tools, the system prompt section, the daemon connection's
// lifetime, and a status widget in pi's terminal UI. Without the controlroom environment it
// does nothing at all, so loading it outside controlroom has no effect.

import { AgentClient } from './client.ts';
import type { ControlroomEnv } from './env.ts';
import { readEnv } from './env.ts';
import type { AgentState } from './protocol/AgentState.ts';
import { PROMPT, SECTION } from './prompt.ts';
import { TOOLS, type Tool, type ToolResult } from './tools.ts';

/** The part of pi's extension API the extension uses (pi 1.2.0's `ExtensionAPI`). */
export interface PiApi {
  registerTool(tool: PiTool): void;
  on(event: 'session_start' | 'session_shutdown', handler: (event: Record<string, unknown>, ctx: PiContext) => void): () => void;
  on(event: 'before_agent_start', handler: (event: BeforeAgentStart, ctx: PiContext) => void): () => void;
}

/** The part of pi's extension context the extension uses. */
export interface PiContext {
  mode?: string;
  ui?: { setWidget?(key: string, lines: string[] | undefined): void };
}

/** pi's `before_agent_start` event: prompt sections are mutable. */
export interface BeforeAgentStart {
  systemPromptOptions?: { sections?: Record<string, string> };
}

/** A tool as pi registers it. */
export interface PiTool {
  name: string;
  label: string;
  description: string;
  promptSnippet: string;
  parameters: object;
  execute(toolCallId: string, params: unknown): Promise<ToolResult<unknown>>;
  renderCall?: unknown;
  renderResult?: unknown;
}

/** Optional renderers per tool name, for pi's terminal UI. */
export type Renderers = Record<string, { renderCall?: unknown; renderResult?: unknown }>;

/** The extension's client name in `hello`. */
export const CLIENT_NAME = '@controlroom/pi';

/** Activates the extension; returns the client, or `null` outside controlroom. */
export function activate(
  pi: PiApi,
  env: Record<string, string | undefined>,
  renderers: Renderers = {},
  connect?: ConstructorParameters<typeof AgentClient>[0]['connect'],
): AgentClient | null {
  const found: ControlroomEnv | null = readEnv(env);
  if (!found) return null;
  const client = new AgentClient({ socket: found.socket, token: found.token, clientName: CLIENT_NAME, connect });
  for (const tool of TOOLS as readonly Tool<unknown, unknown>[]) {
    pi.registerTool({
      name: tool.name,
      label: tool.label,
      description: tool.description,
      promptSnippet: tool.promptSnippet,
      parameters: tool.parameters,
      execute: (toolCallId, params) => tool.execute(client, toolCallId, params),
      ...renderers[tool.name],
    });
  }
  let unsubscribe: (() => void) | null = null;
  pi.on('session_start', (_event, ctx) => {
    client.start();
    unsubscribe?.();
    unsubscribe = ctx.mode === 'tui' ? client.onState((state) => ctx.ui?.setWidget?.(SECTION, widget(state))) : null;
  });
  pi.on('session_shutdown', () => {
    unsubscribe?.();
    unsubscribe = null;
    client.close();
  });
  pi.on('before_agent_start', (event) => {
    const options = event.systemPromptOptions;
    if (options) options.sections = { ...options.sections, [SECTION]: PROMPT };
  });
  return client;
}

/** The terminal UI's status line: todo progress, open questions, auto-continue. */
export function widget(state: AgentState): string[] | undefined {
  const parts: string[] = [];
  const counted = state.todos.filter((t) => t.status !== 'skipped');
  if (counted.length) parts.push(`todo ${counted.filter((t) => t.status === 'done').length}/${counted.length}`);
  if (state.questions.length) parts.push(`${state.questions.length} open question${state.questions.length === 1 ? '' : 's'}`);
  if (counted.length && state.auto_continue.status !== 'on') parts.push(`auto-continue ${state.auto_continue.status}`);
  return parts.length ? [`controlroom: ${parts.join(' · ')}`] : undefined;
}
