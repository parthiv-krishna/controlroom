// The `job` tool (spec §8, §9): background jobs supervised by controlroom under the session's
// sandbox. `run` returns the job id and its log path; the job's exit always wakes the agent,
// and `follow` also wakes it on new (or matching) lines. There is no logs action: the agent
// reads the plain-text log file with tail, grep or sed.

import type { FollowFrom } from './protocol/FollowFrom.ts';
import type { Job } from './protocol/Job.ts';
import type { Tool, ToolResult } from './tools.ts';

/** `job` params. */
export interface JobParams {
  action: 'run' | 'follow' | 'unfollow' | 'status' | 'kill';
  command?: string;
  cwd?: string;
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
  /** `run`: where the log goes. */
  log_path?: string;
}

const text = (s: string) => [{ type: 'text' as const, text: s }];

export const job: Tool<JobParams, JobDetails> = {
  name: 'job',
  label: 'Job',
  description:
    'Background jobs for long-running commands (builds, test suites, servers, watchers), run ' +
    "under this session's sandbox. Actions: run {command, cwd?, timeout? (seconds), name?} " +
    'returns the job id and its log path at once; follow {job_id, buffer_seconds? (default 15), ' +
    'pattern? (regex, e.g. "ERROR|FAIL|panicked"), from? ("unseen" default, or "now")} also wakes ' +
    'you when new or matching lines appear (batched for buffer_seconds); unfollow {job_id}; ' +
    'status {job_id?}; kill {job_id}. The exit of a job always wakes you with its exit code, ' +
    'duration, line count and log path. Output never enters your context: read the log ' +
    "file with tail, grep or sed -n 'a,bp'. Don't poll or sleep waiting for a job; work on " +
    'something else or end your turn.',
  promptSnippet: 'job: run long commands in the background (run, follow, unfollow, status, kill); their exit wakes you',
  parameters: {
    type: 'object',
    properties: {
      action: { type: 'string', enum: ['run', 'follow', 'unfollow', 'status', 'kill'] },
      command: { type: 'string', description: 'The shell command (run).' },
      cwd: { type: 'string', description: "Working directory, inside the session's (run)." },
      timeout: { type: 'integer', minimum: 1, description: 'Stop the job after this many seconds (run).' },
      name: { type: 'string', description: 'A short display name (run).' },
      job_id: { type: 'string', description: 'The job (follow, unfollow, kill; status of one job).' },
      buffer_seconds: { type: 'integer', minimum: 0, description: 'Batch new lines this long before waking you (follow).' },
      pattern: { type: 'string', description: 'Only wake on lines matching this regex (follow).' },
      from: { type: 'string', enum: ['unseen', 'now'], description: 'Cover output not reported yet (default), or only new output (follow).' },
    },
    required: ['action'],
  },
  async execute(client, callId, params): Promise<ToolResult<JobDetails>> {
    const call_id = callId;
    switch (params.action) {
      case 'run': {
        const command = params.command?.trim();
        if (!command) throw new Error('job run needs a command.');
        const started = await client.call('job.run', {
          call_id,
          command,
          ...(params.cwd ? { cwd: params.cwd } : {}),
          ...(params.timeout ? { timeout_s: params.timeout } : {}),
          ...(params.name ? { name: params.name } : {}),
        });
        const said =
          `Job ${started.job_id} started; its log is ${started.log_path}. Its exit will wake you. ` +
          'Follow it to be woken on new or matching lines too. Keep working meanwhile, or end your turn.';
        return { content: text(said), details: { action: 'run', jobs: [], log_path: started.log_path } };
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
        const said = job.follow
          ? `Following job ${job.id}: new ${job.follow.pattern ? `lines matching /${job.follow.pattern}/` : 'lines'} wake you after ${job.follow.buffer_seconds}s. ${describe(job)}`
          : `Job ${job.id} has ended, so there is nothing to follow. ${describe(job)}`;
        return { content: text(said), details: { action: 'follow', jobs: [job] } };
      }
      case 'unfollow': {
        const job = await client.call('job.unfollow', { call_id, job_id: needId(params) });
        return { content: text(`Not following job ${job.id}; its exit still wakes you. ${describe(job)}`), details: { action: 'unfollow', jobs: [job] } };
      }
      case 'kill': {
        const job = await client.call('job.kill', { call_id, job_id: needId(params) });
        const said = job.status === 'running' ? `Stopping job ${job.id} (SIGTERM, then SIGKILL).` : `Job ${job.id} had already ended. ${describe(job)}`;
        return { content: text(said), details: { action: 'kill', jobs: [job] } };
      }
      case 'status': {
        const { jobs } = await client.call('job.status', params.job_id ? { job_id: params.job_id } : {});
        const said = jobs.length ? jobs.map(describe).join('\n') : 'No jobs in this session.';
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

/** One job as the model reads it. */
export function describe(j: Job): string {
  const outcome =
    j.status === 'running'
      ? `running${j.follow ? `, followed${j.follow.pattern ? ` /${j.follow.pattern}/` : ''}` : ''}`
      : `${j.status.replace('_', ' ')}${j.exit !== null ? ` (exit ${j.exit})` : j.signal ? ` (${j.signal})` : ''}`;
  return `Job ${j.id} \`${j.name}\`: ${outcome}, ${j.lines} line${j.lines === 1 ? '' : 's'}, log ${j.log_path}`;
}
