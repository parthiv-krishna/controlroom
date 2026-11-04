<script lang="ts">
  // One terminal: xterm.js (loaded on first use) on a terminal stream from the transport.
  // - Output resets on a fresh replay; the size follows the element (fit) and goes to the PTY.
  // - The agent's commands are marked: the shell integration prints each one (dimmed) before its
  //   OSC 133 `C` marker, and that line gets a tinted background.
  // - Keys: every key goes to the PTY except the pane navigator's Ctrl-h/j/k/l, which pass through
  //   while a program on the pass-through list is in the foreground, or after Ctrl-\ (see keys.ts;
  //   the global key handler reads PASS_ATTR).
  import { onMount } from 'svelte';
  import type { Terminal as XtermTerminal } from '@xterm/xterm';
  import { useApp } from '../app/app.svelte';
  import type { LinkStatus, TerminalLink, TerminalTarget } from './link';
  import { ctrlChar, isPrefix, passesKeys } from './keys';

  let {
    target,
    shellState,
    focusN = 0,
    status = $bindable<LinkStatus>({ state: 'connecting' }),
    foreground = $bindable<string | null>(null),
    prefix = $bindable(false),
    ctrl = $bindable(false),
  }: {
    target: TerminalTarget;
    /** The shell's state from the session summary: a gone shell that comes back reattaches. */
    shellState: string;
    /** Bumped to focus the terminal. */
    focusN?: number;
    status?: LinkStatus;
    foreground?: string | null;
    /** Ctrl-\ was pressed: the next key goes to the terminal. */
    prefix?: boolean;
    /** The phone's sticky Ctrl: the next typed character is sent as its control character. */
    ctrl?: boolean;
  } = $props();

  const app = useApp();
  let host: HTMLDivElement | undefined = $state();
  let term = $state.raw<XtermTerminal | null>(null);
  let link: TerminalLink | null = null;
  const passing = $derived(passesKeys(foreground) || prefix);

  const THEME = { background: '#16181d', foreground: '#d7dae0', cursor: '#d7dae0', selectionBackground: '#3a3f4b' };
  const AGENT_LINE = '#1d2a40';
  const MODIFIERS = new Set(['Control', 'Shift', 'Alt', 'Meta']);

  export function focus(): void {
    term?.focus();
  }

  /** Attaches again after the stream ended. */
  export function reconnect(): void {
    link?.reconnect();
  }

  /** Types into the terminal (the phone's extra keys). */
  export function send(data: string): void {
    link?.input(data);
  }

  onMount(() => {
    let gone = false;
    let observer: ResizeObserver | null = null;
    void (async () => {
      const { Terminal, FitAddon } = await import('./xterm');
      if (gone || !host) return;
      const t = new Terminal({
        theme: THEME,
        fontFamily: '"JetBrains Mono", Menlo, Consolas, "DejaVu Sans Mono", "Liberation Mono", monospace',
        fontSize: 13,
        scrollback: 5000,
        allowProposedApi: true,
      });
      const fit = new FitAddon();
      t.loadAddon(fit);
      t.open(host);
      term = t;
      t.parser.registerOscHandler(133, (data) => {
        if (data.startsWith('C;')) markAgentLine(t);
        return false;
      });
      t.attachCustomKeyEventHandler((e) => {
        // Modifier presses alone (the Ctrl of the next Ctrl-l) neither arm nor use the prefix.
        if (e.type !== 'keydown' || MODIFIERS.has(e.key)) return true;
        if (prefix) {
          prefix = false;
          return true;
        }
        if (isPrefix(e)) {
          prefix = true;
          return false;
        }
        return true;
      });
      link = app.client.transport.openTerminal(target, {
        reset: () => t.reset(),
        output: (data) => t.write(data),
        foreground: (name) => (foreground = name),
        status: (s) => (status = s),
      });
      t.onData((data) => {
        const control = ctrl ? ctrlChar(data) : null;
        if (control) ctrl = false;
        link?.input(control ?? data);
      });
      t.onBinary((data) => link?.input(Uint8Array.from(data, (c) => c.charCodeAt(0))));
      t.onResize(({ cols, rows }) => link?.resize(cols, rows));
      const refit = () => {
        try {
          fit.fit();
        } catch {
          // hidden: nothing to measure
        }
      };
      refit();
      link.resize(t.cols, t.rows);
      observer = new ResizeObserver(refit);
      observer.observe(host);
    })();
    return () => {
      gone = true;
      observer?.disconnect();
      link?.close();
      term?.dispose();
      link = null;
      term = null;
    };
  });

  /** Tints the line above the cursor: the agent's command, printed before its OSC 133 C. */
  function markAgentLine(t: XtermTerminal): void {
    const marker = t.registerMarker(-1);
    const decoration = t.registerDecoration({ marker, width: t.cols, backgroundColor: AGENT_LINE, layer: 'bottom' });
    decoration?.onRender((el) => {
      el.title = 'Run by the agent';
      el.classList.add('xt-agent');
    });
  }

  $effect(() => {
    if (focusN) term?.focus();
  });

  // A terminal whose shell ended attaches again once the shell is back (the agent's next
  // command reopens it).
  let endedAt: string | null = null;
  $effect(() => {
    if (status.state !== 'ended') endedAt = null;
    else if (endedAt === null) endedAt = shellState;
    else if (shellState !== endedAt && shellState !== 'gone') link?.reconnect();
  });
</script>

<!-- data-term-pass is PASS_ATTR (keys.ts): the global key handler leaves Ctrl-h/j/k/l to us. -->
<div class="xt" data-term-pass={passing ? '' : undefined}>
  {#if prefix}<span class="xt-prefix">Ctrl-\ · the next key goes to the terminal</span>{/if}
  <div class="xt-host" bind:this={host} aria-label="Terminal {target.shell}"></div>
</div>

<style>
  .xt { position: relative; flex: 1; min-height: 0; background: #16181d; padding: 4px 0 0 6px; display: flex; }
  .xt-host { flex: 1; min-width: 0; min-height: 0; }
  .xt-host :global(.xterm) { height: 100%; }
  .xt-host :global(.xt-agent) { box-shadow: inset 3px 0 0 #5b8def; pointer-events: none; }
  .xt-prefix { position: absolute; right: 10px; top: 6px; z-index: 5; font-size: 11.5px; padding: 1px 8px; border-radius: 999px; background: #5b8def; color: #fff; }
</style>
