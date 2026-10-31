<script lang="ts" module>
  export interface Suggestion {
    value: string;
    label: string;
    hint?: string;
    /** Offers creating something new ("+ New task …"). */
    isNew?: boolean;
    /** A directory-like entry: Tab or › browses into it. */
    drill?: boolean;
    disabled?: boolean;
    /** A dimmed prefix shown before the label (e.g. `host:`). */
    dim?: string;
  }
</script>

<script lang="ts">
  // Multiple values as chips, with autocomplete. Enter adds the highlighted suggestion (an empty
  // field lets Enter reach the form), Tab browses into a directory-like suggestion, Backspace on an
  // empty field removes the last chip, Esc closes the list.
  import type { Snippet } from 'svelte';

  let {
    values = $bindable(),
    suggest,
    chip,
    label,
    placeholder = '',
    empty = '',
    foot = '',
    mono = false,
  }: {
    values: string[];
    suggest: (q: string, chosen: string[]) => Suggestion[] | Promise<Suggestion[]>;
    chip: Snippet<[string]>;
    label: string;
    placeholder?: string;
    empty?: string;
    foot?: string;
    mono?: boolean;
  } = $props();

  const listId = $props.id();
  let q = $state('');
  let open = $state(false);
  let hi = $state(0);
  let items = $state<Suggestion[]>([]);
  let input: HTMLInputElement | undefined = $state();
  let request = 0;

  async function refresh() {
    const n = ++request;
    const found = await suggest(q, values);
    if (n !== request) return;
    items = found;
    hi = Math.min(hi, Math.max(0, found.length - 1));
  }
  $effect(() => {
    void q;
    void values.length;
    if (open) void refresh();
  });

  function commit(s: Suggestion | undefined) {
    if (!s || s.disabled) return;
    if (!values.includes(s.value)) values = [...values, s.value];
    q = '';
    hi = 0;
    input?.focus();
  }
  function drill(s: Suggestion) {
    q = s.value.endsWith(':') || s.value.endsWith('/') ? s.value : s.value + '/';
    hi = 0;
    input?.focus();
  }
  function onkey(e: KeyboardEvent) {
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
    } else if (e.key === 'Enter' && !e.shiftKey && q.trim()) {
      stop();
      commit(items[hi]);
    } else if (e.key === 'Tab' && open && items[hi]?.drill && !e.shiftKey) {
      stop();
      drill(items[hi]);
    } else if (e.key === 'Backspace' && !q && values.length) {
      stop();
      values = values.slice(0, -1);
    } else if (e.key === 'Escape' && open && items.length) {
      stop();
      open = false;
    }
  }
</script>

<div class="ti" class:focus={open}>
  <div class="box" role="presentation" onmousedown={(e) => { if (e.target !== input) { e.preventDefault(); input?.focus(); } }}>
    {#each values as v (v)}
      <span class="chip" class:mono>{@render chip(v)}<button type="button" aria-label="Remove" onmousedown={(e) => { e.preventDefault(); values = values.filter((x) => x !== v); }}>×</button></span>
    {/each}
    <input
      class:mono
      bind:this={input}
      bind:value={q}
      {placeholder}
      aria-label={label}
      role="combobox"
      aria-controls={listId}
      aria-expanded={open}
      aria-autocomplete="list"
      autocomplete="off"
      spellcheck="false"
      onfocus={() => (open = true)}
      onblur={() => setTimeout(() => (open = false), 120)}
      oninput={() => { open = true; hi = 0; }}
      onkeydown={onkey}
    />
  </div>
  {#if open && (items.length || empty || foot)}
    <div class="dd" id={listId} role="listbox" aria-label={label}>
      {#each items as s, i (s.value)}
        <div class="opt" role="option" tabindex="-1" aria-selected={i === hi} aria-disabled={s.disabled} class:hi={i === hi} class:new={s.isNew} class:dis={s.disabled} onmousedown={(e) => { e.preventDefault(); commit(s); }}>
          <span class="l" class:mono>{#if s.dim}<span class="dim">{s.dim}</span>{/if}{s.label}{#if s.drill}<span class="dim">/</span>{/if}</span>
          {#if s.hint}<span class="h">{s.hint}</span>{/if}
          {#if s.drill}<button type="button" class="drill" title="Browse into (Tab)" aria-label="Browse into" onmousedown={(e) => { e.preventDefault(); e.stopPropagation(); drill(s); }}>›</button>{/if}
        </div>
      {:else}
        {#if empty}<div class="empty-l">{empty}</div>{/if}
      {/each}
      {#if foot}<div class="foot">{foot}</div>{/if}
    </div>
  {/if}
</div>

<style>
  .ti { position: relative; color: var(--fg); }
  .box { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; min-height: 38px; padding: 4px 6px; border: 1px solid var(--line); border-radius: 8px; background: var(--bg2); cursor: text; }
  .focus .box { border-color: var(--accent); }
  .chip { display: inline-flex; align-items: center; gap: 2px; background: var(--bg3); border-radius: 6px; padding: 2px 3px 2px 8px; font-size: 13px; white-space: nowrap; max-width: 100%; }
  .chip button { background: none; border: none; color: var(--muted); cursor: pointer; font: inherit; padding: 0 4px; border-radius: 4px; }
  .chip button:hover { color: var(--fg); }
  input { flex: 1; min-width: 140px; border: none; background: none; padding: 4px 2px; outline: none; }
  input:focus { outline: none; }
  .mono { font-family: var(--mono); font-size: 12.5px; }
  .dd { position: absolute; left: 0; right: 0; top: calc(100% + 4px); z-index: 120; background: var(--bg2); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); max-height: 280px; overflow-y: auto; padding: 4px; }
  .opt { display: flex; align-items: center; gap: 10px; padding: 6px 8px; border-radius: 6px; cursor: pointer; font-size: 13px; }
  .opt.hi { background: var(--bg3); }
  .opt.new .l { color: var(--info); }
  .opt.dis { opacity: 0.5; cursor: default; }
  .l { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .h { font-size: 11.5px; color: var(--muted); white-space: nowrap; }
  .dim { color: var(--muted); }
  .drill { background: none; border: 1px solid var(--line); color: var(--muted); border-radius: 5px; width: 22px; height: 22px; cursor: pointer; font: inherit; line-height: 1; padding: 0; }
  .drill:hover { color: var(--fg); }
  .empty-l { padding: 8px; font-size: 12.5px; color: var(--muted); }
  .foot { border-top: 1px solid var(--line); margin-top: 4px; padding: 6px 8px 3px; font-size: 11.5px; color: var(--muted); }
</style>
