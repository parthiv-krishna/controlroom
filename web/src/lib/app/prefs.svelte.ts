// Preferences that persist in localStorage: grouping, pane pins and sizes, theme, settings shape,
// keyboard options. App.svelte saves them whenever one changes.
import type { GroupBy } from '../model/grouping';

export type Theme = 'light' | 'dark' | 'system';

export interface KeyboardPrefs {
  /** Ctrl-h/j/k/l moves focus between panes. */
  navigator: boolean;
  /** Vim keys in lists: j/k, h/l, gg/G, /, n/N, y, `:`. */
  listVim: boolean;
}

interface Stored {
  group: GroupBy;
  leftPin: boolean;
  topPin: boolean;
  notesPin: boolean;
  listWidth: number;
  notesWidth: number;
  /** 0: the default (44% of the viewport). */
  boardHeight: number;
  theme: Theme;
  settingsPage: boolean;
  keyboard: KeyboardPrefs;
}

const KEY = 'cr-prefs';
const DEFAULTS: Stored = {
  group: 'task',
  leftPin: true,
  topPin: false,
  notesPin: false,
  listWidth: 360,
  notesWidth: 360,
  boardHeight: 0,
  theme: 'system',
  settingsPage: false,
  keyboard: { navigator: true, listVim: true },
};

function load(): Stored {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Stored>) } : DEFAULTS;
  } catch {
    return DEFAULTS;
  }
}

export class Prefs {
  group = $state<GroupBy>(DEFAULTS.group);
  leftPin = $state(DEFAULTS.leftPin);
  topPin = $state(DEFAULTS.topPin);
  notesPin = $state(DEFAULTS.notesPin);
  listWidth = $state(DEFAULTS.listWidth);
  notesWidth = $state(DEFAULTS.notesWidth);
  boardHeight = $state(DEFAULTS.boardHeight);
  theme = $state<Theme>(DEFAULTS.theme);
  settingsPage = $state(DEFAULTS.settingsPage);
  keyboard = $state<KeyboardPrefs>(DEFAULTS.keyboard);

  constructor() {
    Object.assign(this, load());
  }

  /** Reads every field (so an effect calling it re-runs on any change) and stores them. */
  save(): void {
    const value: Stored = {
      group: this.group,
      leftPin: this.leftPin,
      topPin: this.topPin,
      notesPin: this.notesPin,
      listWidth: this.listWidth,
      notesWidth: this.notesWidth,
      boardHeight: this.boardHeight,
      theme: this.theme,
      settingsPage: this.settingsPage,
      keyboard: { ...this.keyboard },
    };
    try {
      localStorage.setItem(KEY, JSON.stringify(value));
    } catch {
      // storage disabled: preferences last for this visit
    }
  }
}
