// `bash`, `shell_new` and `shell_list` (design §5, spec §9): commands run in persistent, named
// shells on the workspaces' machines (cwd, env and venvs persist per shell). Output streams
// into pi's tool updates while the command runs; an abort interrupts the command on the host
// (Ctrl-C into its terminal) and returns the partial output. Every result starts with
// `[shell devbox · devbox:~/src/api · exit 0]`.

import type { AgentShell } from '../protocol/AgentShell.ts';
import type { SessionInfo } from '../protocol/SessionInfo.ts';
import { text, type Tool, type ToolCall } from '../tool.ts';
import { MAX_BYTES, MAX_LINES } from './format.ts';
import { pathParam } from './files.ts';
import { hostCall, linkDropNote } from './host.ts';
import { listWorkspaces, parseTarget, shown } from './target.ts';

const WAITING_NOTE = '[The command is waiting for terminal input. The user can type it in the session terminal; you cannot.]';

/** `bash` params: pi's shape plus the shell. */
export interface BashParams {
  command: string;
  shell?: string;
  /** Seconds. */
  timeout?: number;
}

/** `bash` details (pi's renderer shows `fullOutputPath`). */
export type BashDetails = { fullOutputPath: string } | undefined;

export const bash: Tool<BashParams, BashDetails> = {
  name: 'bash',
  label: 'bash',
  description:
    'Run a bash command in a named, persistent shell on a machine. cd, exported variables, functions and activated venvs persist between calls in the same shell. ' +
    'Without shell, it runs in the primary workspace\'s default shell (see <workspaces>). The result starts with [shell · cwd · exit]. ' +
    `Output is the last ${MAX_LINES} lines or ${MAX_BYTES / 1024}KB (the full output's path is given when cut). ` +
    'Commands cannot read terminal input: one that waits for it is reported, and the user can type in the session terminal. Default timeout 600 seconds.',
  promptSnippet: 'Execute bash commands in persistent named shells on the workspaces\' machines',
  parameters: {
    type: 'object',
    properties: {
      command: { type: 'string', description: 'Bash command to execute' },
      shell: { type: 'string', description: "The shell's name (see <workspaces>; default: the primary workspace's shell)" },
      timeout: { type: 'number', description: 'Timeout in seconds (default 600)' },
    },
    required: ['command'],
  },
  async execute(call, params) {
    const info = await call.client.sessionInfo();
    const name = params.shell?.trim() || defaultShell(info);
    const machine = await shellMachine(call, info, name);
    let output = '';
    let waiting = false;
    const update = () => {
      const tail = output.length > MAX_BYTES ? output.slice(-MAX_BYTES) : output;
      call.onUpdate?.({ content: text(waiting ? `${tail}\n${WAITING_NOTE}` : tail), details: undefined });
    };
    call.onUpdate?.({ content: [], details: undefined });
    const { result: r, linkDrops } = await hostCall(
      call,
      info,
      machine,
      { op: 'bash', shell: name, command: params.command, ...(params.timeout ? { timeout_s: params.timeout } : {}) },
      {
        interruptible: true,
        onOutput: (chunk) => {
          output += chunk;
          update();
        },
        onWaiting: (now) => {
          waiting = now;
          update();
        },
      },
    );
    const log = shown(info, machine, r.log_path);
    const notes = [
      r.shell_reset && `[Shell ${name} was reopened before this command (${r.shell_reset}): its variables, functions and venv were lost.]`,
      r.truncated && `[Output cut to the last ${MAX_LINES} lines or ${MAX_BYTES / 1024}KB. Full output: ${log}]`,
      r.timed_out && `[Command timed out after ${params.timeout ?? 600} seconds and was interrupted.]`,
      r.cancelled && '[Command aborted]',
      r.waited_for_input && '[The command waited for terminal input.]',
      linkDropNote(machine, linkDrops),
    ].filter((n): n is string => Boolean(n));
    const status = r.exit_code ?? r.signal ?? '?';
    const header = `[shell ${name} · ${shown(info, machine, r.cwd)} · exit ${status}]`;
    const body = [header, r.output.replace(/\n+$/, ''), notes.join('\n')].filter(Boolean).join('\n');
    const failed = r.exit_code !== 0 || r.timed_out || r.cancelled;
    return { content: text(body), details: r.truncated ? { fullOutputPath: log } : undefined, ...(failed ? { isError: true } : {}) };
  },
};

/** The primary workspace's default shell. */
function defaultShell(info: SessionInfo): string {
  const primary = info.workspaces.find((w) => w.primary) ?? info.workspaces[0];
  if (!primary) throw new Error('This session has no workspaces yet, so there is no shell to run in.');
  return primary.shell;
}

/** The machine a shell runs on: from the session info, else asked again (a new shell). */
async function shellMachine(call: ToolCall, info: SessionInfo, name: string): Promise<string> {
  const known = info.shells.find((s) => s.name === name)?.machine ?? info.workspaces.find((w) => w.shell === name)?.machine;
  if (known) return known;
  const { shells } = await call.client.call('shell.list', {});
  const found = shells.find((s) => s.name === name);
  if (found) return found.machine;
  const names = [...new Set([...shells.map((s) => s.name), ...info.workspaces.map((w) => w.shell)])];
  throw new Error(`There is no shell "${name}". Shells: ${names.join(', ') || 'none'}. Open one with shell_new.`);
}

/** `shell_new` params. */
export interface ShellNewParams {
  name: string;
  at?: string;
}

/** `shell_new` and `shell_list` details: the shells concerned. */
export interface ShellDetails {
  shells: AgentShell[];
}

export const shellNew: Tool<ShellNewParams, ShellDetails> = {
  name: 'shell_new',
  label: 'shell_new',
  description:
    'Open a new named, persistent shell (a fresh login environment) in a directory on a machine, e.g. to run independent commands in parallel ' +
    'or keep a server running. Run commands in it with bash {shell: name}. An open shell with that name is reused.',
  promptSnippet: 'Open another named shell on a machine (bash {shell})',
  parameters: {
    type: 'object',
    properties: {
      name: { type: 'string', description: 'The shell name, unique in this session (e.g. "devbox-tests")' },
      at: pathParam('Its starting directory (default: the workspace, when the session has one)'),
    },
    required: ['name'],
  },
  async execute(call, params) {
    const name = params.name?.trim();
    if (!name) throw new Error('shell_new needs a name.');
    const info = await call.client.sessionInfo();
    const target = parseTarget(info, params.at, 'directory');
    const shell = await call.client.call('shell.open', { call_id: call.id, name, machine: target.machine, cwd: target.path });
    const cwd = shown(info, shell.machine, shell.cwd);
    if (shell.machine !== target.machine) {
      throw new Error(`A shell named "${name}" already exists on ${shell.machine} (in ${cwd}); choose another name.`);
    }
    return { content: text(`Shell ${name} is open on ${shell.machine} in ${cwd}. Use bash {shell: "${name}"}.`), details: { shells: [shell] } };
  },
};

const STATES: Record<string, string> = { idle: 'idle', running: 'running a command', waiting_input: 'waiting for terminal input', gone: 'gone' };

export const shellList: Tool<Record<string, never>, ShellDetails> = {
  name: 'shell_list',
  label: 'shell_list',
  description: "List this session's shells with their machine, current directory and state.",
  promptSnippet: "List the session's shells",
  parameters: { type: 'object', properties: {} },
  async execute(call) {
    const info = await call.client.sessionInfo();
    const { shells } = await call.client.call('shell.list', {});
    if (!shells.length) return { content: text(`No shells yet. ${listWorkspaces(info)}`), details: { shells } };
    const lines = shells.map((s) => {
      const reset = s.reset_pending ? '; reopened, its variables are lost' : '';
      return `- ${s.name}: ${shown(info, s.machine, s.cwd)} (${STATES[s.state.state] ?? s.state.state}${reset})`;
    });
    return { content: text(lines.join('\n')), details: { shells } };
  },
};
