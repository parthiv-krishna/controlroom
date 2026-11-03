// The tools the extension registers (spec §9): `ask_user`, `todo`, `notify_user` and `job` (in
// `job.ts`). Each is a plain definition (name, description, JSON Schema parameters) plus an
// `execute` that talks to the daemon, so they run and test without pi. Results carry
// model-facing text and `details` for rendering.

import type { AgentClient } from './client.ts';
import { job } from './job.ts';
import type { AgentQuestion } from './protocol/AgentQuestion.ts';
import type { NoticeLevel } from './protocol/NoticeLevel.ts';
import type { Todo } from './protocol/Todo.ts';
import type { TodoStatus } from './protocol/TodoStatus.ts';

/** What a tool returns to pi. */
export interface ToolResult<D> {
  content: { type: 'text'; text: string }[];
  details: D;
}

/** A tool independent of pi's types. */
export interface Tool<P, D> {
  name: string;
  label: string;
  description: string;
  promptSnippet: string;
  parameters: object;
  execute(client: AgentClient, callId: string, params: P): Promise<ToolResult<D>>;
}

const text = (s: string) => [{ type: 'text' as const, text: s }];

// ---------------------------------------------------------------------------------------------
// ask_user
// ---------------------------------------------------------------------------------------------

/** One question as the model writes it. */
export interface AskQuestion {
  question: string;
  options?: (string | { label: string; description?: string })[];
  multi?: boolean;
  free_text?: boolean;
}

/** `ask_user` params: ask questions, or withdraw one. */
export interface AskParams {
  questions?: AskQuestion[];
  title?: string;
  withdraw?: string;
}

/** `ask_user` details. */
export type AskDetails =
  | { action: 'ask'; question_id: string; questions: string[] }
  | { action: 'withdraw'; question_id: string; state: string };

export const askUser: Tool<AskParams, AskDetails> = {
  name: 'ask_user',
  label: 'Ask user',
  description:
    'Ask the user one or more questions. Never blocks: it returns at once with a question id, ' +
    'and the answer arrives later as a message that wakes you. Keep working on other things ' +
    'meanwhile. Offer short options when the answer is a choice. To withdraw an open question ' +
    'that no longer matters, call it with {"withdraw": "<question id>"}.',
  promptSnippet: 'ask_user: ask the user questions without blocking; the answer wakes you later',
  parameters: {
    type: 'object',
    properties: {
      questions: {
        type: 'array',
        minItems: 1,
        description: 'The questions, answered together.',
        items: {
          type: 'object',
          properties: {
            question: { type: 'string', description: 'The question (Markdown).' },
            options: {
              type: 'array',
              description: 'Suggested answers.',
              items: {
                anyOf: [
                  { type: 'string' },
                  {
                    type: 'object',
                    properties: { label: { type: 'string' }, description: { type: 'string' } },
                    required: ['label'],
                  },
                ],
              },
            },
            multi: { type: 'boolean', description: 'Several options may be picked.' },
            free_text: { type: 'boolean', description: 'A typed answer is allowed (default true).' },
          },
          required: ['question'],
        },
      },
      title: { type: 'string', description: 'A one-line summary (default: the first question).' },
      withdraw: { type: 'string', description: 'Withdraw this open question instead of asking.' },
    },
  },
  async execute(client, callId, params) {
    if (params.withdraw) {
      const result = await client.call('question.withdraw', { call_id: callId, question_id: params.withdraw });
      const said =
        result.state === 'withdrawn'
          ? `Question ${params.withdraw} withdrawn.`
          : `Question ${params.withdraw} was already ${result.state}${result.answer ? ` (answer: ${result.answer.label}); the answer is delivered to you` : ''}.`;
      return { content: text(said), details: { action: 'withdraw', question_id: params.withdraw, state: result.state } };
    }
    const questions = (params.questions ?? []).map(toQuestion);
    if (!questions.length) throw new Error('ask_user needs at least one question (or "withdraw").');
    const result = await client.call('question.ask', {
      call_id: callId,
      ...(params.title ? { title: params.title } : {}),
      questions,
    });
    return {
      content: text(`Question ${result.question_id} sent. ${result.reminder}`),
      details: { action: 'ask', question_id: result.question_id, questions: questions.map((q) => q.text) },
    };
  },
};

function toQuestion(q: AskQuestion): AgentQuestion {
  const options = (q.options ?? []).map((o) => (typeof o === 'string' ? { label: o } : { label: o.label, ...(o.description ? { description: o.description } : {}) }));
  return {
    text: q.question,
    options,
    ...(q.multi !== undefined ? { multi: q.multi } : {}),
    ...(q.free_text !== undefined ? { free_text: q.free_text } : {}),
  };
}

// ---------------------------------------------------------------------------------------------
// todo
// ---------------------------------------------------------------------------------------------

/** `todo` params. */
export interface TodoParams {
  action: 'list' | 'add' | 'update' | 'complete' | 'block';
  items?: string[];
  id?: string | number;
  text?: string;
  status?: TodoStatus;
  reason?: string;
}

/** `todo` details: the list after the call. */
export interface TodoDetails {
  action: TodoParams['action'];
  todos: Todo[];
  changed: string[];
}

const STATUSES = ['pending', 'in_progress', 'done', 'blocked', 'skipped'];

export const todo: Tool<TodoParams, TodoDetails> = {
  name: 'todo',
  label: 'Todo',
  description:
    "The session's shared todo list; the user sees and edits it too. Actions: list; add " +
    '{items}; update {id, text?, status?, reason?}; complete {id}; block {id, reason}. ' +
    'Statuses: pending, in_progress, done, blocked, skipped. An id is the entry id or its ' +
    'number in the list. While pending or in-progress entries remain, controlroom restarts ' +
    'you after your turn ends (auto-continue): mark entries done as you finish them, and block ' +
    'them (with the reason) when you need the user.',
  promptSnippet: 'todo: the shared todo list (list, add, update, complete, block)',
  parameters: {
    type: 'object',
    properties: {
      action: { type: 'string', enum: ['list', 'add', 'update', 'complete', 'block'] },
      items: { type: 'array', items: { type: 'string' }, description: 'New entries (add).' },
      id: { anyOf: [{ type: 'string' }, { type: 'integer' }], description: 'The entry id, or its number in the list.' },
      text: { type: 'string', description: 'New text (update).' },
      status: { type: 'string', enum: STATUSES, description: 'New status (update).' },
      reason: { type: 'string', description: 'Why it is blocked or skipped.' },
    },
    required: ['action'],
  },
  async execute(client, callId, params) {
    const call_id = callId;
    let todos: Todo[];
    let changed: string[] = [];
    switch (params.action) {
      case 'list':
        todos = (await client.call('todo.list', {})).todos;
        break;
      case 'add': {
        const texts = (params.items ?? (params.text ? [params.text] : [])).map((t) => t.trim()).filter(Boolean);
        if (!texts.length) throw new Error('todo add needs items.');
        todos = (await client.call('todo.add', { call_id, texts })).todos;
        changed = texts;
        break;
      }
      case 'update':
      case 'complete':
      case 'block': {
        const todo_id = await resolveId(client, params.id);
        const status = params.action === 'complete' ? 'done' : params.action === 'block' ? 'blocked' : params.status;
        if (params.action === 'block' && !params.reason?.trim()) throw new Error('todo block needs a reason.');
        if (status && !STATUSES.includes(status)) throw new Error(`unknown status ${status}`);
        todos = (
          await client.call('todo.update', {
            call_id,
            todo_id,
            ...(params.text ? { text: params.text } : {}),
            ...(status ? { status } : {}),
            ...(params.reason !== undefined ? { reason: params.reason } : {}),
          })
        ).todos;
        changed = todos.filter((t) => t.id === todo_id).map((t) => t.text);
        break;
      }
      default:
        throw new Error(`unknown todo action ${String((params as { action: unknown }).action)}`);
    }
    return { content: text(renderTodos(todos)), details: { action: params.action, todos, changed } };
  },
};

/** An entry id, or a 1-based number resolved against the current list. */
async function resolveId(client: AgentClient, id: string | number | undefined): Promise<string> {
  if (id === undefined || id === '') throw new Error('this todo action needs an id.');
  const n = typeof id === 'number' ? id : /^\d+$/.test(id.trim()) ? Number(id) : null;
  if (n === null) return String(id).trim();
  const todos = client.state?.todos ?? (await client.call('todo.list', {})).todos;
  const entry = todos[n - 1];
  if (!entry) throw new Error(`there is no todo #${n} (the list has ${todos.length}).`);
  return entry.id;
}

const MARK: Record<string, string> = { pending: ' ', in_progress: '~', done: 'x', blocked: '!', skipped: '-' };

/** The list as the model reads it. */
export function renderTodos(todos: Todo[]): string {
  if (!todos.length) return 'The todo list is empty.';
  const open = todos.filter((t) => t.status === 'pending' || t.status === 'in_progress').length;
  const lines = todos.map((t, i) => {
    const by = t.updated_by.kind === 'user' ? ' (edited by the user)' : '';
    const reason = t.reason ? ` - ${t.reason}` : '';
    return `${i + 1}. [${MARK[t.status] ?? '?'}] ${t.text}${reason} {id: ${t.id}, ${t.status}}${by}`;
  });
  return `Todo list (${open} open):\n${lines.join('\n')}`;
}

// ---------------------------------------------------------------------------------------------
// notify_user
// ---------------------------------------------------------------------------------------------

/** `notify_user` params. */
export interface NotifyParams {
  text: string;
  level?: NoticeLevel;
}

/** `notify_user` details. */
export interface NotifyDetails {
  text: string;
  level: NoticeLevel;
}

export const notifyUser: Tool<NotifyParams, NotifyDetails> = {
  name: 'notify_user',
  label: 'Notify user',
  description:
    "Show the user a short notice in the session's timeline (progress, a heads-up, a result " +
    'worth seeing). It asks nothing; use ask_user for questions.',
  promptSnippet: 'notify_user: show the user a notice without asking anything',
  parameters: {
    type: 'object',
    properties: {
      text: { type: 'string', description: 'The notice (Markdown).' },
      level: { type: 'string', enum: ['info', 'warning', 'error'] },
    },
    required: ['text'],
  },
  async execute(client, callId, params) {
    const level = params.level ?? 'info';
    if (!params.text?.trim()) throw new Error('notify_user needs text.');
    await client.call('notify', { call_id: callId, text: params.text, level });
    return { content: text('Notice shown to the user.'), details: { text: params.text, level } };
  },
};

/** Every tool. */
export const TOOLS = [askUser, todo, notifyUser, job] as const;
