// A pi stand-in that records registrations and lets tests fire events and run tools, plus a
// harness that activates the extension against a fake daemon.

import { activate, type PiApi, type PiContext, type PiTool } from '../src/extension.ts';
import type { SessionInfo } from '../src/protocol/SessionInfo.ts';
import type { ToolResult } from '../src/tool.ts';
import type { FakeDaemon } from './fake-daemon.ts';

/** Options of one tool run. */
export interface RunOptions {
  callId?: string;
  signal?: AbortSignal;
  onUpdate?: (partial: ToolResult<unknown>) => void;
}

/** A tool's result with its text joined, for assertions. */
export type Ran = ToolResult<unknown> & { text: string };

export class FakePi implements PiApi {
  tools = new Map<string, PiTool>();
  handlers = new Map<string, ((event: never, ctx: PiContext) => unknown)[]>();

  registerTool(tool: PiTool) {
    this.tools.set(tool.name, tool);
  }

  on(event: string, handler: (event: never, ctx: PiContext) => unknown) {
    this.handlers.set(event, [...(this.handlers.get(event) ?? []), handler]);
    return () => {};
  }

  /** Fires an event and waits for its (possibly async) handlers. */
  async fire(event: string, payload: object, ctx: PiContext = { mode: 'rpc' }): Promise<void> {
    for (const h of this.handlers.get(event) ?? []) await h(payload as never, ctx);
  }

  /** Runs a tool like pi: arguments prepared, then executed. */
  async run(name: string, params: object, options: RunOptions = {}): Promise<Ran> {
    const tool = this.tools.get(name);
    if (!tool) throw new Error(`no tool ${name}`);
    const args = tool.prepareArguments ? tool.prepareArguments(params) : params;
    const result = await tool.execute(options.callId ?? 'call-1', args, options.signal, options.onUpdate);
    const text = result.content.map((c) => (c.type === 'text' ? c.text : `<${c.mimeType}>`)).join('\n');
    return { ...result, text };
  }
}

/** Activates the extension against `daemon`, which serves `info` as `session.info`. */
export async function started(daemon: FakeDaemon, info?: SessionInfo): Promise<FakePi> {
  if (info) daemon.handlers.set('session.info', () => ({ result: info }));
  const pi = new FakePi();
  activate(pi, { CONTROLROOM_SESSION_ID: 'S', CONTROLROOM_AGENT_SOCKET: daemon.socket, CONTROLROOM_AGENT_TOKEN: daemon.token });
  await pi.fire('session_start', {});
  return pi;
}
