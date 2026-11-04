import { describe, expect, it } from 'vitest';
import type { Shell, Workspace } from '../protocol';
import { FixtureTerminals } from './fixture';
import { ctrlChar, passesKeys, terminalTakesKeys } from './keys';
import type { LinkStatus } from './link';
import { shellGroups, type DockSession } from './session';

describe('terminal keys', () => {
  it('passes the navigator keys to full-screen programs only', () => {
    expect(passesKeys('vim')).toBe(true);
    expect(passesKeys('lazygit')).toBe(true);
    expect(passesKeys('sleep')).toBe(false);
    expect(passesKeys(null)).toBe(false);
  });

  it('finds a pass-through terminal around the focused element', () => {
    document.body.innerHTML = '<div data-term-pass=""><textarea id="in"></textarea></div><textarea id="out"></textarea>';
    expect(terminalTakesKeys(document.getElementById('in'))).toBe(true);
    expect(terminalTakesKeys(document.getElementById('out'))).toBe(false);
    expect(terminalTakesKeys(null)).toBe(false);
  });

  it('maps characters to control characters', () => {
    expect(ctrlChar('c')).toBe('\x03');
    expect(ctrlChar('C')).toBe('\x03');
    expect(ctrlChar('[')).toBe('\x1b');
    expect(ctrlChar('1')).toBeNull();
    expect(ctrlChar('ab')).toBeNull();
  });
});

const ws = (id: string, machine_id: string, path: string, primary: boolean): Workspace => ({
  id,
  machine_id,
  path,
  root: null,
  primary,
  added_at: 0,
  availability: { connectivity: { state: 'connected' }, since: 0 },
});
const shell = (name: string, workspace_id: string): Shell => ({ name, workspace_id, cwd: '~', state: { state: 'idle' }, reset_pending: false, foreground: null });

describe('dock sessions', () => {
  it('groups shells by workspace, primary first', () => {
    const s: DockSession = {
      id: 's',
      workspaces: [ws('w1', 'm1', '~/api', true), ws('w2', 'm2', '~/web', false), ws('w3', 'm1', '~/empty', false)],
      shells: [shell('buildbox', 'w2'), shell('devbox', 'w1'), shell('devbox-2', 'w1'), shell('stray', 'w9')],
    };
    const names = { m1: 'devbox', m2: 'buildbox' } as Record<string, string>;
    const groups = shellGroups(s, (id) => names[id]);
    expect(groups.map((g) => [g.label, g.shells.map((x) => x.name)])).toEqual([
      ['devbox:~/api', ['devbox', 'devbox-2']],
      ['buildbox:~/web', ['buildbox']],
      ['other', ['stray']],
    ]);
  });
});

describe('fixture terminals', () => {
  it('echo, run vim in the foreground, and record input and resizes', async () => {
    const terminals = new FixtureTerminals();
    let screen = '';
    const status: LinkStatus[] = [];
    const foreground: (string | null)[] = [];
    const link = terminals.open(
      { session: 's', shell: 'devbox' },
      {
        reset: () => (screen = ''),
        output: (d) => (screen += new TextDecoder().decode(d)),
        foreground: (n) => foreground.push(n),
        status: (s) => status.push(s),
      },
    );
    await Promise.resolve();
    expect(status.map((s) => s.state)).toEqual(['connecting', 'live']);
    expect(screen).toContain('cargo test -q');
    link.resize(120, 40);
    link.input('stty size\r');
    expect(screen).toContain('40 120');
    link.input('vim\r');
    link.input(':q\r');
    expect(foreground).toEqual([null, 'vim', null]);
    link.input('sudo true\rs3cret\r');
    expect(screen).toContain('authenticated');
    expect(screen).not.toContain('s3cret');
    expect(terminals.log[0]).toMatchObject({ inputs: ['stty size\r', 'vim\r', ':q\r', 'sudo true\rs3cret\r'], resizes: [[120, 40]] });
  });
});
