// A scripted OpenAI-compatible model on loopback for the real-pi test: each chat completion
// request gets the next scripted reply (tool calls or text), streamed as SSE chunks. Requests
// are recorded so the test can check the tools pi declared and the system prompt it built.

import { createServer, type IncomingMessage, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

/** One scripted reply: tool calls, or a final text. */
export type Step = { tools: { id: string; name: string; args: object }[] } | { text: string };

/** What pi sent (the parts the test reads). */
export interface ModelRequest {
  messages: { role: string; content?: unknown; tool_call_id?: string }[];
  tools?: { function: { name: string } }[];
}

export class MockModel {
  readonly requests: ModelRequest[] = [];
  #steps: Step[];
  #server: Server;
  #waiters: (() => void)[] = [];

  private constructor(steps: Step[]) {
    this.#steps = [...steps];
    this.#server = createServer((req, res) => {
      void body(req).then((raw) => {
        if (req.method !== 'POST' || !req.url?.endsWith('/chat/completions')) {
          res.writeHead(404).end();
          return;
        }
        this.requests.push(JSON.parse(raw));
        for (const wake of this.#waiters.splice(0)) wake();
        const step = this.#steps.shift() ?? { text: 'Done.' };
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        for (const frame of frames(step, this.requests.length)) res.write(`data: ${JSON.stringify(frame)}\n\n`);
        res.end('data: [DONE]\n\n');
      });
    });
  }

  static async start(steps: Step[]): Promise<MockModel> {
    const model = new MockModel(steps);
    await new Promise<void>((resolve) => model.#server.listen(0, '127.0.0.1', resolve));
    return model;
  }

  /** The OpenAI base URL (`…/v1`). */
  get baseUrl(): string {
    return `http://127.0.0.1:${(this.#server.address() as AddressInfo).port}/v1`;
  }

  /** Adds replies for later requests. */
  script(...steps: Step[]): void {
    this.#steps.push(...steps);
  }

  /** Resolves once `n` requests arrived. */
  async requestCount(n: number): Promise<void> {
    while (this.requests.length < n) await new Promise<void>((resolve) => this.#waiters.push(resolve));
  }

  async stop(): Promise<void> {
    this.#server.closeAllConnections();
    await new Promise<void>((resolve) => this.#server.close(() => resolve()));
  }
}

function body(req: IncomingMessage): Promise<string> {
  return new Promise((resolve) => {
    let data = '';
    req.setEncoding('utf8');
    req.on('data', (chunk: string) => (data += chunk));
    req.on('end', () => resolve(data));
  });
}

/** The `chat.completion.chunk` frames of one reply. */
function frames(step: Step, n: number): object[] {
  const chunk = (delta: object, finish: string | null = null, usage?: object) => ({
    id: `mock-${n}`,
    object: 'chat.completion.chunk',
    created: 1_700_000_000,
    model: 'mock-model',
    choices: [{ index: 0, delta, finish_reason: finish }],
    ...(usage ? { usage } : {}),
  });
  const usage = { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 };
  if ('text' in step) return [chunk({ role: 'assistant', content: step.text }), chunk({}, 'stop', usage)];
  return [
    ...step.tools.flatMap((t, index) => [
      chunk({ role: 'assistant', tool_calls: [{ index, id: t.id, type: 'function', function: { name: t.name, arguments: '' } }] }),
      chunk({ tool_calls: [{ index, function: { arguments: JSON.stringify(t.args) } }] }),
    ]),
    chunk({}, 'tool_calls', usage),
  ];
}
