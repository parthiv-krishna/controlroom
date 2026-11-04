// Wires the extension into pi: the tools, the system prompt, the daemon connection's lifetime,
// and a status widget in pi's terminal UI. Without the controlroom environment it does nothing
// at all, so loading it outside controlroom has no effect.
//
// pi runs on the controlroom server with `--no-builtin-tools`: the file and shell tools here
// replace pi's built-ins under the same names and argument shapes (paths as `machine:path`),
// so pi's renderers, the timeline's tool mapping and the model's habits carry over.

import { AgentClient } from './client.ts';
import { readEnv } from './env.ts';
import { job } from './job.ts';
import type { AgentState } from './protocol/AgentState.ts';
import { PROMPT, SECTION } from './prompt.ts';
import { edit, read, write } from './remote/files.ts';
import { type ContextFile, contextFiles, CWD, type PromptSkill, skills, WORKSPACES_SECTION, workspacesSection } from './remote/prompt.ts';
import { find, grep, ls } from './remote/search.ts';
import { bash, shellList, shellNew } from './remote/shell.ts';
import type { Tool, ToolResult } from './tool.ts';
import { askUser, notifyUser, todo } from './tools.ts';

/** The part of pi's extension API the extension uses (pi 1.2.0's `ExtensionAPI`). */
export interface PiApi {
  registerTool(tool: PiTool): void;
  on(event: 'session_start' | 'session_shutdown', handler: (event: Record<string, unknown>, ctx: PiContext) => void): () => void;
  on(event: 'before_agent_start', handler: (event: BeforeAgentStart, ctx: PiContext) => Promise<void>): () => void;
}

/** The part of pi's extension context the extension uses. */
export interface PiContext {
  mode?: string;
  ui?: { setWidget?(key: string, lines: string[] | undefined): void };
}

/** pi's `before_agent_start` event: the prompt's options are mutable. */
export interface BeforeAgentStart {
  systemPromptOptions?: {
    sections?: Record<string, string>;
    cwd?: string;
    contextFiles?: ContextFile[];
    skills?: PromptSkill[];
  };
}

/** A tool as pi registers it. */
export interface PiTool {
  name: string;
  label: string;
  description: string;
  promptSnippet: string;
  promptGuidelines?: string[];
  parameters: object;
  prepareArguments?: (args: unknown) => unknown;
  execute(
    toolCallId: string,
    params: unknown,
    signal?: AbortSignal,
    onUpdate?: (partial: ToolResult<unknown>) => void,
    ctx?: unknown,
  ): Promise<ToolResult<unknown>>;
  renderShell?: unknown;
  renderCall?: unknown;
  renderResult?: unknown;
}

/** Optional renderers per tool name, for pi's terminal UI. */
export type Renderers = Record<string, { renderShell?: unknown; renderCall?: unknown; renderResult?: unknown }>;

/** The extension's client name in `hello`. */
export const CLIENT_NAME = '@controlroom/pi';

/** Every tool, in the order the model sees them. */
export const TOOLS = [read, write, edit, bash, shellNew, shellList, grep, find, ls, job, askUser, todo, notifyUser] as const;

/** `<workspaces>` while the daemon can't say what they are. */
const NO_INFO = "The session's workspaces could not be loaded (the controlroom daemon did not answer). File and shell tools say so until it is back.";

/** Activates the extension; returns the client, or `null` outside controlroom. */
export function activate(
  pi: PiApi,
  env: Record<string, string | undefined>,
  renderers: Renderers = {},
  connect?: ConstructorParameters<typeof AgentClient>[0]['connect'],
): AgentClient | null {
  const found = readEnv(env);
  if (!found) return null;
  const client = new AgentClient({ socket: found.socket, token: found.token, clientName: CLIENT_NAME, connect });
  for (const tool of TOOLS as readonly Tool<unknown, unknown>[]) {
    pi.registerTool({
      name: tool.name,
      label: tool.label,
      description: tool.description,
      promptSnippet: tool.promptSnippet,
      ...(tool.promptGuidelines ? { promptGuidelines: tool.promptGuidelines } : {}),
      ...(tool.prepareArguments ? { prepareArguments: tool.prepareArguments } : {}),
      parameters: tool.parameters,
      execute: (id, params, signal, onUpdate, ctx) => tool.execute({ client, id, signal, onUpdate, ctx }, params),
      ...renderers[tool.name],
    });
  }
  let unsubscribe: (() => void) | null = null;
  pi.on('session_start', (_event, ctx) => {
    client.start();
    unsubscribe?.();
    unsubscribe = ctx.mode === 'tui' ? client.on('state', (state) => ctx.ui?.setWidget?.(SECTION, widget(state))) : null;
  });
  pi.on('session_shutdown', () => {
    unsubscribe?.();
    unsubscribe = null;
    client.close();
  });
  pi.on('before_agent_start', async (event) => {
    const options = event.systemPromptOptions;
    if (!options) return;
    const sections: Record<string, string> = { ...options.sections, [SECTION]: PROMPT };
    options.sections = sections;
    options.cwd = CWD;
    const info = await client.sessionInfo().catch(() => null);
    if (!info) {
      sections[WORKSPACES_SECTION] = NO_INFO;
      return;
    }
    options.contextFiles = contextFiles(info);
    options.skills = skills(info);
    sections[WORKSPACES_SECTION] = workspacesSection(info);
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
