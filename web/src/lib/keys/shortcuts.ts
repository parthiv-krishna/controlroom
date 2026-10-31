// The shortcut list shown by `?` and in Settings › Keyboard.
export const SHORTCUTS: [string, string][] = [
  ['Ctrl-h/j/k/l', 'Move focus to the pane left / below / above / right'],
  ['⌘K · Ctrl-Shift-K · :', 'Command palette (: for commands such as :approve, :new, :goto)'],
  ['j / k', 'Next / previous session'],
  ['h / l', 'Collapse / expand the current session’s subagents'],
  ['gg / G', 'First / last session'],
  ['Enter', 'Focus the composer'],
  ['/', 'Search the list'],
  ['n / N', 'Next / previous match of the last search'],
  ['y', 'Copy the session’s host:path'],
  ['gr', 'Cycle grouping'],
  ['e', 'Archive / unarchive'],
  ['[  t  ]', 'Toggle the session list / tasks / notifications'],
  ['Alt-0…9', 'Switch project'],
  ['Esc', 'Stop the running turn (composer), close, or go back'],
  ['?', 'This help'],
];
