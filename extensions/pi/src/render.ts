// How the tools look in pi's terminal UI (and HTML exports). The remote file and shell tools
// reuse pi's own renderers (`$ cmd`, edit diffs, `read path:range`), since they keep pi's
// names, argument shapes and details; the others get one line per call and a compact result.
// Loaded by pi only; the tests don't need it.

import {
  createBashToolDefinition,
  createEditToolDefinition,
  createFindToolDefinition,
  createGrepToolDefinition,
  createLsToolDefinition,
  createReadToolDefinition,
  createWriteToolDefinition,
} from '@earendil-works/pi-coding-agent';
import { Text } from '@earendil-works/pi-tui';

import type { Renderers } from './extension.ts';
import type { JobDetails, JobParams } from './job.ts';
import type { ShellDetails, ShellNewParams } from './remote/shell.ts';
import type { AskDetails, AskParams, NotifyDetails, NotifyParams, TodoDetails, TodoParams } from './tools.ts';

interface Theme {
  fg(color: string, text: string): string;
  bold(text: string): string;
}

interface Result<D> {
  content: { type: string; text?: string }[];
  details?: D;
}

const MARK: Record<string, [string, string]> = {
  pending: ['○', 'dim'],
  in_progress: ['◐', 'accent'],
  done: ['●', 'success'],
  blocked: ['⊘', 'error'],
  skipped: ['–', 'dim'],
};

const firstText = (result: Result<unknown>) => result.content.find((c) => c.type === 'text')?.text ?? '';

/** pi's renderers of one built-in tool. */
const piRenderers = ({ renderShell, renderCall, renderResult }: { renderShell?: unknown; renderCall?: unknown; renderResult?: unknown }) => ({
  renderShell,
  renderCall,
  renderResult,
});

export const renderers: Renderers = {
  read: piRenderers(createReadToolDefinition('/')),
  write: piRenderers(createWriteToolDefinition('/')),
  edit: piRenderers(createEditToolDefinition('/')),
  bash: piRenderers(createBashToolDefinition('/')),
  grep: piRenderers(createGrepToolDefinition('/')),
  find: piRenderers(createFindToolDefinition('/')),
  ls: piRenderers(createLsToolDefinition('/')),
  shell_new: {
    renderCall: (args: ShellNewParams, theme: Theme) =>
      new Text(theme.fg('toolTitle', theme.bold('shell_new ')) + theme.fg('muted', `${args.name ?? ''} ${args.at ?? ''}`.trim()), 0, 0),
    renderResult: (result: Result<ShellDetails>, _options: unknown, theme: Theme) => new Text(theme.fg('muted', firstText(result)), 0, 0),
  },
  shell_list: {
    renderCall: (_args: unknown, theme: Theme) => new Text(theme.fg('toolTitle', theme.bold('shell_list')), 0, 0),
    renderResult: (result: Result<ShellDetails>, _options: unknown, theme: Theme) => new Text(theme.fg('muted', firstText(result)), 0, 0),
  },
  ask_user: {
    renderCall: (args: AskParams, theme: Theme) =>
      new Text(
        theme.fg('toolTitle', theme.bold('ask_user ')) +
          (args.withdraw ? theme.fg('muted', `withdraw ${args.withdraw}`) : theme.fg('muted', (args.questions ?? []).map((q) => q.question).join(' · '))),
        0,
        0,
      ),
    renderResult: (result: Result<AskDetails>, _options: unknown, theme: Theme) => {
      const d = result.details;
      if (!d) return new Text(firstText(result), 0, 0);
      const line = d.action === 'ask' ? `? sent ${d.question_id}; the answer will wake the agent` : `⊘ ${d.question_id}: ${d.state}`;
      return new Text(theme.fg('muted', line), 0, 0);
    },
  },
  todo: {
    renderCall: (args: TodoParams, theme: Theme) => {
      const what = args.items?.join(', ') ?? (args.id !== undefined ? `#${args.id}` : '');
      return new Text(theme.fg('toolTitle', theme.bold('todo ')) + theme.fg('muted', `${args.action} ${what}`.trim()), 0, 0);
    },
    renderResult: (result: Result<TodoDetails>, options: { expanded?: boolean }, theme: Theme) => {
      const todos = result.details?.todos;
      if (!todos) return new Text(firstText(result), 0, 0);
      if (!todos.length) return new Text(theme.fg('dim', 'empty'), 0, 0);
      const shown = options.expanded ? todos : todos.slice(0, 8);
      const lines = shown.map((t) => {
        const [mark, color] = MARK[t.status] ?? ['?', 'dim'];
        return `${theme.fg(color, mark)} ${theme.fg(t.status === 'done' || t.status === 'skipped' ? 'dim' : 'text', t.text)}`;
      });
      if (shown.length < todos.length) lines.push(theme.fg('dim', `… ${todos.length - shown.length} more`));
      return new Text(lines.join('\n'), 0, 0);
    },
  },
  notify_user: {
    renderCall: (args: NotifyParams, theme: Theme) => new Text(theme.fg('toolTitle', theme.bold('notify_user ')) + theme.fg('muted', args.text ?? ''), 0, 0),
    renderResult: (result: Result<NotifyDetails>, _options: unknown, theme: Theme) =>
      new Text(theme.fg(result.details?.level === 'error' ? 'error' : result.details?.level === 'warning' ? 'warning' : 'muted', '✓ shown to the user'), 0, 0),
  },
  job: {
    renderCall: (args: JobParams, theme: Theme) => {
      const what = args.action === 'run' ? (args.name ?? args.command ?? '') : (args.job_id ?? '');
      const pattern = args.action === 'follow' && args.pattern ? ` /${args.pattern}/` : '';
      return new Text(theme.fg('toolTitle', theme.bold('job ')) + theme.fg('muted', `${args.action} ${what}${pattern}`.trim()), 0, 0);
    },
    renderResult: (result: Result<JobDetails>, _options: unknown, theme: Theme) => {
      const d = result.details;
      if (!d) return new Text(firstText(result), 0, 0);
      if (d.action === 'run') return new Text(theme.fg('muted', `⚙ started · log ${d.log_path ?? ''}`), 0, 0);
      const lines = d.jobs.map((j) => {
        const color = j.status === 'running' ? 'accent' : j.status === 'succeeded' ? 'success' : 'error';
        return `${theme.fg(color, '⚙')} ${j.name} ${theme.fg('dim', `${j.status.replace('_', ' ')} · ${j.lines} lines${j.follow ? ' · followed' : ''}`)}`;
      });
      return new Text(lines.join('\n') || theme.fg('dim', 'no jobs'), 0, 0);
    },
  },
};
