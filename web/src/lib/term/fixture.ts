// Terminal streams for the fixture build (development and the Playwright smoke tests): a tiny
// scripted shell per stream, with no backend logic. It replays an agent command (marked with OSC
// 133 like the host's shell integration), echoes typing line by line, and knows three commands:
// `vim` (a full-screen program in the foreground until `:q`), `sudo …` (a password prompt that
// doesn't echo) and `stty size`. Every input and resize is recorded for the tests.
import type { LinkStatus, TerminalHandlers, TerminalLink, TerminalTarget } from './link';

const OSC = (body: string) => `\x1b]${body}\x07`;

/** What the fixture recorded of one stream. */
export interface FixtureTerminalLog {
  target: TerminalTarget;
  inputs: string[];
  resizes: [number, number][];
  closed: boolean;
}

export class FixtureTerminals {
  readonly log: FixtureTerminalLog[] = [];

  open(target: TerminalTarget, handlers: TerminalHandlers): TerminalLink {
    const entry: FixtureTerminalLog = { target, inputs: [], resizes: [], closed: false };
    this.log.push(entry);
    return new FakeShell(entry, handlers);
  }
}

class FakeShell implements TerminalLink {
  private readonly encoder = new TextEncoder();
  private line = '';
  private mode: 'shell' | 'vim' | 'secret' = 'shell';
  private size: [number, number] = [80, 24];

  constructor(
    private readonly entry: FixtureTerminalLog,
    private readonly h: TerminalHandlers,
  ) {
    this.attach();
  }

  private get prompt(): string {
    return `${OSC('133;A')}\x1b[1;32malice@${this.entry.target.shell}\x1b[0m:\x1b[1;34m~/src\x1b[0m$ `;
  }

  private write(text: string): void {
    this.h.output(this.encoder.encode(text));
  }

  private status(s: LinkStatus): void {
    this.h.status(s);
  }

  private attach(): void {
    this.status({ state: 'connecting' });
    queueMicrotask(() => {
      if (this.entry.closed) return;
      this.h.reset();
      this.status({ state: 'live' });
      this.h.foreground(null);
      this.write(`${this.prompt}\x1b[2mcargo test -q\x1b[0m\r\n${OSC('133;C;fixture')}test result: ok. 42 passed; 0 failed\r\n${OSC('133;D;0')}${this.prompt}`);
    });
  }

  input(data: string | Uint8Array): void {
    const text = typeof data === 'string' ? data : new TextDecoder().decode(data);
    this.entry.inputs.push(text);
    for (const ch of text) this.key(ch);
  }

  private key(ch: string): void {
    if (ch === '\r') return this.enter();
    if (ch === '\x03') {
      this.line = '';
      this.mode = 'shell';
      this.h.foreground(null);
      return this.write(`^C\r\n${this.prompt}`);
    }
    if (ch === '\x7f') {
      if (this.line && this.mode !== 'secret') this.write('\b \b');
      this.line = this.line.slice(0, -1);
      return;
    }
    if (ch < ' ') return;
    this.line += ch;
    if (this.mode === 'shell') this.write(ch);
  }

  private enter(): void {
    const line = this.line.trim();
    this.line = '';
    if (this.mode === 'vim') {
      if (line !== ':q') return;
      this.mode = 'shell';
      this.h.foreground(null);
      return this.write(`\x1b[?1049l${this.prompt}`);
    }
    if (this.mode === 'secret') {
      this.mode = 'shell';
      return this.write(`\r\nauthenticated\r\n${this.prompt}`);
    }
    this.write('\r\n');
    if (line === 'vim' || line.startsWith('vim ')) {
      this.mode = 'vim';
      this.h.foreground('vim');
      return this.write(`\x1b[?1049h\x1b[H~\r\n~\r\n~ VIM (fixture): type :q and Enter`);
    }
    if (line.startsWith('sudo ')) {
      this.mode = 'secret';
      return this.write('[sudo] password for alice: ');
    }
    if (line === 'stty size') this.write(`${this.size[1]} ${this.size[0]}\r\n`);
    else if (line) this.write(`${line.split(' ')[0]}: ran in the fixture\r\n`);
    this.write(this.prompt);
  }

  resize(cols: number, rows: number): void {
    this.size = [cols, rows];
    this.entry.resizes.push([cols, rows]);
  }

  reconnect(): void {
    this.attach();
  }

  close(): void {
    this.entry.closed = true;
  }
}
