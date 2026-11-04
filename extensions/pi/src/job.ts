// The `job` tool (spec §8, §9): background jobs on a workspace's machine, under that
// workspace's sandbox. `run` returns the job id and its log path; the job's exit always wakes
// the agent, and `follow` also wakes it on new (or matching) lines. There is no logs action:
// the agent reads the plain-text log file (`machine:path`) with read or grep.

import type { FollowFrom } from './protocol/FollowFrom.ts';
import type { Job } from './protocol/Job.ts';
import type { SessionInfo } from './protocol/SessionInfo.ts';
import { pathParam } from './remote/files.ts';
import { parseTarget, shown } from './remote/target.ts';
import { text, type Tool, type ToolResult } from './tool.ts';

/** `job` params. */
export interface JobParams {
  action: 'run' | 'follow' | 'unfollow' | 'status' | 'kill';
  command?: string;
  /** `machine:path`: where the job runs. */
  at?: string;
  /** Seconds. */
  timeout?: number;
  name?: string;
  job_id?: string;
  buffer_seconds?: number;
  pattern?: string;
  from?: FollowFrom;
}

/** `job` details: the jobs the call is about. */
export interface JobDetails {
  action: JobParams['action'];
  jobs: Job[];
  /** `run`: where the log goes, as `machine:path`. */
  log_path?: string;
}

export const job: Tool<JobParams, JobDetails> = {
  name: 'job',
  label: 'Job',
  description:
    "Background jobs for long-running commands (builds, test suites, servers, watchers), run on a workspace's machine under its sandbox; " +
    'they keep running while the link or the server is down. Actions: run {command, at? (machine:path, the working directory), ' +
    'timeout? (seconds), name?} returns the job id and its log path at once; follow {job_id, buffer_seconds? (default 15), ' +
    'pattern? (regex, e.g. "ERROR|FAIL|panicked"), from? ("unseen" default, or "now")} also wakes you when new or matching lines appear ' +
    '(batched for buffer_seconds); unfollow {job_id}; status {job_id?}; kill {job_id}. The exit of a job always wakes you with its ' +
    "exit code, duration, line count and log path. Output never enters your context: read the log (machine:path) with read or grep. Don't " +
    'poll or sleep waiting for a job; work on something else or end your turn.',
  promptSnippet: 'job: run long commands in the background (run, follow, unfollow, status, kill); their exit wakes you',
  parameters: {
    type: 'object',
    properties: {
      action: { type: 'string', enum: ['run', 'follow', 'unfollow', 'status', 'kill'] },
      command: { type: 'string', description: 'The shell command (run).' },
      at: pathParam('The working directory (run; default: the workspace, when the session has one)'),
      timeout: { type: 'integer', minimum: 1, description: 'Stop the job after this many seconds (run).' },
      name: { type: 'string', description: 'A short display name (run).' },
      job_id: { type: 'string', description: 'The job (follow, unfollow, kill; status of one job).' },
      buffer_seconds: { type: 'integer', minimum: 0, description: 'Batch new lines this long before waking you (follow).' },
      pattern: { type: 'string', description: 'Only wake on lines matching this regex (follow).' },
      from: { type: 'string', enum: ['unseen', 'now'], description: 'Cover output not reported yet (default), or only new output (follow).' },
    },
    required: ['action'],
  },
  async execute({ client, id: call_id }, params): Promise<ToolResult<JobDetails>> {
    switch (params.action) {
      case 'run': {
        const command = params.command?.trim();
        if (!command) throw new Error('job run needs a command.');
        const info = await client.sessionInfo();
        const at = parseTarget(info, params.at, 'directory');
        const started = await client.call('job.run', {
          call_id,
          command,
          machine: at.machine,
          ...(at.path ? { cwd: at.path } : {}),
          ...(params.timeout ? { timeout_s: params.timeout } : {}),
          ...(params.name ? { name: params.name } : {}),
        });
        const log = shown(info, at.machine, started.log_path);
        const said =
          `Job ${started.job_id} started on ${at.machine}; its log is ${log}. Its exit will wake you. ` +
          'Follow it to be woken on new or matching lines too. Keep working meanwhile, or end your turn.';
        return { content: text(said), details: { action: 'run', jobs: [], log_path: log } };
      }
      case 'follow': {
        const job_id = needId(params);
        const job = await client.call('job.follow', {
          call_id,
          job_id,
          ...(params.buffer_seconds !== undefined ? { buffer_seconds: params.buffer_seconds } : {}),
          ...(params.pattern ? { pattern: params.pattern } : {}),
          ...(params.from ? { from: params.from } : {}),
        });
        const info = await client.sessionInfo();
        const said = job.follow
          ? `Following job ${job.id}: new ${job.follow.pattern ? `lines matching /${job.follow.pattern}/` : 'lines'} wake you after ${job.follow.buffer_seconds}s. ${describe(job, info)}`
          : `Job ${job.id} has ended, so there is nothing to follow. ${describe(job, info)}`;
        return { content: text(said), details: { action: 'follow', jobs: [job] } };
      }
      case 'unfollow': {
        const job = await client.call('job.unfollow', { call_id, job_id: needId(params) });
        const said = `Not following job ${job.id}; its exit still wakes you. ${describe(job, await client.sessionInfo())}`;
        return { content: text(said), details: { action: 'unfollow', jobs: [job] } };
      }
      case 'kill': {
        const job = await client.call('job.kill', { call_id, job_id: needId(params) });
        const said =
          job.status === 'running' ? `Stopping job ${job.id} (SIGTERM, then SIGKILL).` : `Job ${job.id} had already ended. ${describe(job, await client.sessionInfo())}`;
        return { content: text(said), details: { action: 'kill', jobs: [job] } };
      }
      case 'status': {
        const { jobs } = await client.call('job.status', params.job_id ? { job_id: params.job_id } : {});
        const info = await client.sessionInfo();
        const said = jobs.length ? jobs.map((j) => describe(j, info)).join('\n') : 'No jobs in this session.';
        return { content: text(said), details: { action: 'status', jobs } };
      }
      default:
        throw new Error(`unknown job action ${String((params as { action: unknown }).action)}`);
    }
  },
};

function needId(params: JobParams): string {
  const id = params.job_id?.trim();
  if (!id) throw new Error(`job ${params.action} needs a job_id.`);
  return id;
}

/** One job as the model reads it, its log as `machine:path`. */
export function describe(j: Job, info: SessionInfo): string {
  const outcome =
    j.status === 'running'
      ? `running${j.follow ? `, followed${j.follow.pattern ? ` /${j.follow.pattern}/` : ''}` : ''}`
      : `${j.status.replace('_', ' ')}${j.exit !== null ? ` (exit ${j.exit})` : j.signal ? ` (${j.signal})` : ''}`;
  return `Job ${j.id} \`${j.name}\`: ${outcome}, ${j.lines} line${j.lines === 1 ? '' : 's'}, log ${logDisplay(j, info)}`;
}

/**
 * A job's log as `machine:path`. Logs live under their workspace's root
 * (`<root>/.controlroom/jobs/`), which names the machine; the raw path when no single
 * workspace matches.
 */
function logDisplay(j: Job, info: SessionInfo): string {
  const owners = info.workspaces.filter((w) => w.root && j.log_path.startsWith(`${w.root}/.controlroom/`));
  const machines = new Set(owners.map((w) => w.machine));
  return machines.size === 1 ? shown(info, owners[0].machine, j.log_path) : j.log_path;
}
