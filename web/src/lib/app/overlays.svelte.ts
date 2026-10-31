// In-app replacements for window.confirm/prompt, and toasts. No native dialogs anywhere.
import type { SessionRef } from './view.svelte';

export interface DialogRequest {
  kind: 'confirm' | 'ask';
  title: string;
  body?: string;
  value?: string;
  placeholder?: string;
  hint?: string;
  ok?: string;
  danger?: boolean;
  multiline?: boolean;
}

export class Dialogs {
  current = $state<(DialogRequest & { resolve: (v: string | boolean | null) => void }) | null>(null);

  confirm(o: Omit<DialogRequest, 'kind'>): Promise<boolean> {
    return this.open({ ...o, kind: 'confirm' }).then((v) => v === true);
  }

  /** Resolves to the trimmed text, or null when cancelled or empty. */
  ask(o: Omit<DialogRequest, 'kind'>): Promise<string | null> {
    return this.open({ ...o, kind: 'ask' }).then((v) => (typeof v === 'string' && v.trim() ? v.trim() : null));
  }

  close(value: string | boolean | null): void {
    const d = this.current;
    this.current = null;
    d?.resolve(value);
  }

  private open(request: DialogRequest): Promise<string | boolean | null> {
    this.current?.resolve(null);
    return new Promise((resolve) => (this.current = { ...request, resolve }));
  }
}

export interface Toast {
  id: number;
  text: string;
  level: 'info' | 'ok' | 'warn' | 'error';
  /** Offers an Open button. */
  session?: SessionRef;
}

export class Toasts {
  list = $state<Toast[]>([]);
  private next = 1;

  show(text: string, level: Toast['level'] = 'info', session?: SessionRef): void {
    const id = this.next++;
    this.list.push({ id, text, level, session });
    setTimeout(() => this.dismiss(id), 4500);
  }

  dismiss(id: number): void {
    this.list = this.list.filter((t) => t.id !== id);
  }
}
