// Global keyboard handling (spec §13, keyboard everywhere). The capture handler runs before any
// field: the pane navigator and the palette work from text fields too. The bubble handler takes
// the list shortcuts when no field, dialog or modal has the keyboard.
import type { App } from '../app/app.svelte';
import { displayName } from '../model/filters';
import { primaryWorkspace, wsLabel } from '../model/workspaces';
import { moveFocus } from './navigator';

const inField = (t: EventTarget | null): t is HTMLElement =>
  t instanceof HTMLElement && (['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName) || t.isContentEditable);

/** Ctrl-h/j/k/l and ⌘K / Ctrl-Shift-K, from anywhere. */
export function onCaptureKey(app: App, e: KeyboardEvent): void {
  if (app.dialogs.current) return;
  const k = e.key.toLowerCase();
  if (k === 'k' && !e.altKey && ((e.metaKey && !e.ctrlKey) || (e.ctrlKey && e.shiftKey))) {
    e.preventDefault();
    e.stopPropagation();
    app.view.palette = app.view.palette === null ? '' : null;
    return;
  }
  if (!app.prefs.keyboard.navigator || !e.ctrlKey || e.metaKey || e.altKey || e.shiftKey || !'hjkl'.includes(k) || k.length !== 1) return;
  if (app.view.palette !== null || app.view.modal || app.view.settings) return;
  e.preventDefault();
  e.stopPropagation();
  moveFocus(k as 'h' | 'j' | 'k' | 'l');
}

let pendingG = 0;

export function onKey(app: App, e: KeyboardEvent): void {
  const { view, prefs } = app;
  if (e.defaultPrevented || app.dialogs.current || view.modal || view.settings || view.palette !== null) return;
  if (inField(e.target)) {
    if (e.key === 'Escape') e.target.blur();
    return;
  }
  if (e.metaKey || e.ctrlKey) return;
  const vim = prefs.keyboard.listVim;
  const cur = app.current;
  if (e.key === '?') return void (e.preventDefault(), (view.help = !view.help));
  if (e.key === ':' && vim) return void (e.preventDefault(), (view.palette = ':'));
  if (e.altKey) {
    if (/^[0-9]$/.test(e.key)) {
      const p = [null, ...app.client.feed.projectList][Number(e.key)];
      if (p !== undefined) app.openProject(p?.id ?? null);
    }
    return;
  }
  if (vim && pendingG && Date.now() - pendingG < 800) {
    pendingG = 0;
    if (e.key === 'g') return first(app);
    if (e.key === 'r') return app.cycleGroup();
  }
  switch (e.key) {
    case 'j': return app.move(1);
    case 'k': return app.move(-1);
    case 'g':
      if (vim) pendingG = Date.now();
      else app.cycleGroup();
      return;
    case 'G': {
      const list = app.orderedSessions();
      if (vim && list.length) app.open(list[list.length - 1]);
      return;
    }
    case 'h':
    case 'l':
      if (vim && cur) view.collapsed['c:' + cur.id] = e.key === 'h';
      return;
    case 'n':
    case 'N':
      if (vim && view.lastSearch) return nextMatch(app, e.key === 'n' ? 1 : -1);
      if (e.key === 'n') app.toggleNotes();
      return;
    case 'y': {
      const w = cur && primaryWorkspace(cur);
      if (vim && w) void app.copy(wsLabel(w, app.client.feed.machines));
      return;
    }
    case '/': {
      e.preventDefault();
      document.getElementById('session-search')?.focus();
      return;
    }
    case '[': return app.toggleLeft();
    case ']': return app.toggleNotes();
    case 't':
      if (app.desk) app.toggleTop();
      else view.page = 'tasks';
      return;
    case 'e':
      if (cur) void (cur.state === 'closed' ? app.unarchive(cur) : app.archive(cur));
      return;
    case 'Enter':
      if (cur) {
        e.preventDefault();
        document.getElementById(`composer-${cur.id}`)?.focus();
      }
      return;
    case 'Escape':
      if (view.menu) view.menu = null;
      else if (view.openTask) view.openTask = null;
      else if (view.notesOpen) view.notesOpen = false;
      return;
    default:
  }
}

function first(app: App): void {
  const list = app.orderedSessions();
  if (list.length) app.open(list[0]);
}

function nextMatch(app: App, dir: 1 | -1): void {
  const q = app.view.lastSearch.toLowerCase();
  const list = app.orderedSessions();
  const cur = app.view.current;
  const i = cur ? list.findIndex((s) => s.id === cur.id) : -1;
  for (let step = 1; step <= list.length; step++) {
    const s = list[(i + dir * step + list.length * 2) % list.length];
    if (displayName(s).toLowerCase().includes(q)) return app.open(s);
  }
}
