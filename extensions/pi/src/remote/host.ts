// One host operation through the agent daemon (`host.call`): its output streamed while it
// runs, its abort turned into `host.cancel`, its typed errors turned into messages the model
// can act on. The call id is pi's tool call id (or `<id>/<step>`), so a resend after a
// reconnect is deduplicated end to end; host calls have no client timeout because the daemon
// keeps them open while the host is unreachable (up to the machine's grace).

import { ControlroomError } from '../client.ts';
import type { HostOp } from '../protocol/HostOp.ts';
import type { HostResult } from '../protocol/HostResult.ts';
import type { SessionInfo } from '../protocol/SessionInfo.ts';
import type { ToolCall } from '../tool.ts';
import { listWorkspaces, shown } from './target.ts';

/** How long an interrupted command may take to report its partial result. */
const CANCEL_GRACE_MS = 10_000;

/** A host result of one op kind. */
export type ResultOf<O extends HostOp['op']> = Extract<HostResult, { op: O }>;

/** A host call's outcome. */
export interface HostReply<R> {
  result: R;
  /** How often the link to the host dropped while the call was open (it still ran once). */
  linkDrops: number;
}

/** Options of one host call. */
export interface HostCallOptions {
  /** The step's call id; default the tool call's. */
  id?: string;
  /** On abort, interrupt the command (`host.cancel`) and wait for its partial result. */
  interruptible?: boolean;
  /** New output of a running command. */
  onOutput?: (text: string) => void;
  /** The command started or stopped waiting for terminal input. */
  onWaiting?: (waiting: boolean) => void;
}

/** Runs one op on `machine`. Errors are rewritten for the model (see `hostError`). */
export async function hostCall<O extends HostOp['op']>(
  call: ToolCall,
  info: SessionInfo,
  machine: string,
  op: Extract<HostOp, { op: O }>,
  options: HostCallOptions = {},
): Promise<HostReply<ResultOf<O>>> {
  const { client, signal } = call;
  const call_id = options.id ?? call.id;
  const unsubscribe = [
    options.onOutput && client.on('host.output', (p) => p.call_id === call_id && options.onOutput!(p.text)),
    options.onWaiting && client.on('host.waiting_input', (p) => p.call_id === call_id && options.onWaiting!(p.waiting)),
  ];
  // An interruptible call keeps waiting after the abort, for the partial result, but not forever.
  const giveUp = new AbortController();
  let timer: NodeJS.Timeout | undefined;
  const interrupt = () => {
    client.call('host.cancel', { call_id }).catch(() => {});
    timer = setTimeout(() => giveUp.abort(), CANCEL_GRACE_MS);
    timer.unref();
  };
  if (options.interruptible) signal?.addEventListener('abort', interrupt, { once: true });
  try {
    if (signal?.aborted) throw new ControlroomError('Operation aborted', 'aborted');
    const reply = await client.call(
      'host.call',
      { call_id, machine, op },
      { timeoutMs: null, signal: options.interruptible ? giveUp.signal : signal },
    );
    if (reply.result.op !== op.op) throw new Error(`${machine} answered a ${op.op} call with a ${reply.result.op} result.`);
    return { result: reply.result as ResultOf<O>, linkDrops: reply.link_drops };
  } catch (error) {
    throw hostError(error, info, machine, call_id);
  } finally {
    for (const off of unsubscribe) if (off) off();
    signal?.removeEventListener('abort', interrupt);
    clearTimeout(timer);
  }
}

/** The note for a result that survived link drops; empty when there were none. */
export function linkDropNote(machine: string, drops: number): string {
  if (!drops) return '';
  const times = drops === 1 ? 'once' : `${drops} times`;
  return `[The link to ${machine} dropped ${times} during this call; the result came from the host's journal, so it ran once.]`;
}

/** A host call's failure as a message the model can act on. */
export function hostError(error: unknown, info: SessionInfo, machine: string, callId: string): Error {
  if (!(error instanceof ControlroomError)) return error instanceof Error ? error : new Error(String(error));
  const data = error.data;
  switch (data?.kind) {
    case 'conflict':
      return new Error(
        `${shown(info, machine, data.path)} changed after it was read (another session or the user wrote it), so nothing was written. Read it again and redo the change.`,
      );
    case 'connection_lost':
      return new Error(
        `${machine} is unreachable since ${new Date(data.since).toISOString()}; the call may still be running there (call ${callId}). Its result will be delivered to you when ${machine} is back.`,
      );
    case 'lost':
      return new Error(
        data.may_have_run
          ? `The host daemon on ${machine} restarted while this call ran, and its result is lost; it may have partly run. Check its effects before retrying.`
          : `The host daemon on ${machine} restarted before this call took effect; nothing changed, so retrying is safe.`,
      );
    case 'shell_gone':
      return new Error(`Shell "${data.shell}" is gone. Open it again with shell_new.`);
    case 'not_found':
      return data.resource === 'workspace'
        ? new Error(`"${machine}" is not the machine of one of this session's workspaces. ${listWorkspaces(info)}`)
        : new Error(error.message);
    case 'fs_error':
      return new Error(`${shown(info, machine, data.path)}: ${data.detail}`);
    case 'machine_unavailable':
      return new Error(`${machine} is unavailable (${data.connectivity.state}).`);
    default:
      return error.kind === 'aborted' ? new Error('Operation aborted') : new Error(error.message);
  }
}
