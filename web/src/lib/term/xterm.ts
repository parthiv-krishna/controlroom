// xterm.js and its stylesheet, imported lazily (only when a terminal opens) so they stay out of
// the main bundle.
import '@xterm/xterm/css/xterm.css';

export { Terminal } from '@xterm/xterm';
export { FitAddon } from '@xterm/addon-fit';
