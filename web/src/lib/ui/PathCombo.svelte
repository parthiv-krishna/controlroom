<script lang="ts">
  // The working-directory field: browses the machine's real directories (`fs.list`), recent
  // directories first. Tab or › opens a folder. Remote paths are POSIX strings (`/`, `~`).
  import { useApp } from '../app/app.svelte';
  import type { MachineId } from '../protocol';
  import type { Suggestion } from './TagInput.svelte';

  let { value = $bindable(), machineId, id, recent = [] }: { value: string; machineId: MachineId; id: string; recent?: string[] } = $props();

  const app = useApp();
  const listId = $props.id();
  let open = $state(false);
  let hi = $state(0);
  let items = $state<Suggestion[]>([]);
  let input: HTMLInputElement | undefined = $state();
  let request = 0;

  async function dirs(dir: string): Promise<string[]> {
    const r = await app.client.call('fs.list', { machine_id: machineId, path: dir });
    return (r?.entries ?? []).filter((e) => e.kind === 'dir' || e.kind === 'symlink').map((e) => e.name);
  }
  const join = (dir: string, name: string) => (dir === '/' ? `/${name}` : `${dir}/${name}`);

  async function refresh() {
    const n = ++request;
    const q = value;
    let out: Suggestion[];
    if (!q) {
      out = [...recent.map((p) => ({ value: p, label: p, hint: 'recent' })), { value: '~', label: '~', drill: true }];
    } else {
      const slash = q.lastIndexOf('/');
      const dir = q.endsWith('/') ? (q.length > 1 ? q.slice(0, -1) : '/') : slash < 0 ? '' : slash === 0 ? '/' : q.slice(0, slash);
      const part = q.endsWith('/') ? '' : q.slice(slash + 1);
      out = dir
        ? (await dirs(dir)).filter((name) => name.startsWith(part)).map((name) => ({ value: join(dir, name), label: join(dir, name), drill: true }))
        : '~'.startsWith(part) ? [{ value: '~', label: '~', drill: true }] : [];
    }
    if (n !== request) return;
    items = out;
    hi = Math.min(hi, Math.max(0, out.length - 1));
  }
  $effect(() => {
    void value;
    void machineId;
    if (open && app.client.online) void refresh();
  });

  function set(v: string, keepOpen: boolean) {
    value = v;
    open = keepOpen;
    hi = 0;
    input?.focus();
  }
  function onkey(e: KeyboardEvent) {
    const it = items[hi];
    const stop = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (e.key === 'ArrowDown') {
      stop();
      open = true;
      hi = Math.min(items.length - 1, hi + 1);
    } else if (e.key === 'ArrowUp') {
      stop();
      hi = Math.max(0, hi - 1);
    } else if (e.key === 'Enter' && open && it && it.value !== value) {
      stop();
      set(it.value, false);
    } else if (e.key === 'Tab' && open && it?.drill && !e.shiftKey) {
      stop();
      set(it.value + '/', true);
    } else if (e.key === 'Escape' && open && items.length) {
      stop();
      open = false;
    }
  }
</script>

<div class="combo">
  <input
    {id}
    bind:this={input}
    bind:value
    placeholder="~/src/project"
    autocomplete="off"
    spellcheck="false"
    data-autofocus
    role="combobox"
    aria-controls={listId}
    aria-expanded={open}
    aria-label="Working directory"
    onfocus={() => (open = true)}
    onblur={() => setTimeout(() => (open = false), 120)}
    oninput={() => { open = true; hi = 0; }}
    onkeydown={onkey}
  />
  {#if open && items.length}
    <div class="dd" id={listId} role="listbox" aria-label="Directories">
      {#each items as it, i (it.value)}
        <div class="opt" role="option" tabindex="-1" aria-selected={i === hi} class:hi={i === hi} onmousedown={(e) => { e.preventDefault(); set(it.value, false); }}>
          <span class="l">{it.label}{#if it.drill}<span class="dim">/</span>{/if}</span>
          {#if it.hint}<span class="h">{it.hint}</span>{/if}
          {#if it.drill}<button type="button" class="drill" title="Browse into (Tab)" aria-label="Browse into" onmousedown={(e) => { e.preventDefault(); e.stopPropagation(); set(it.value + '/', true); }}>›</button>{/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .combo { position: relative; }
  input { font-family: var(--mono); font-size: 13px; }
  .dd { position: absolute; left: 0; right: 0; top: calc(100% + 4px); z-index: 120; background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); max-height: 260px; overflow-y: auto; padding: 4px; }
  .opt { display: flex; align-items: center; gap: 10px; padding: 6px 8px; border-radius: 6px; cursor: pointer; }
  .opt.hi { background: var(--bg3); }
  .l { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: 12.5px var(--mono); }
  .h { font-size: 11.5px; color: var(--muted); }
  .dim { color: var(--muted); }
  .drill { background: none; border: 1px solid var(--line); color: var(--muted); border-radius: 5px; width: 22px; height: 22px; cursor: pointer; font: inherit; padding: 0; }
</style>
