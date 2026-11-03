// @controlroom/pi: controlroom's pi extension (spec §9). pi loads this file; see
// `extension.ts` for what it does. Outside a controlroom session it registers nothing.

import { activate, type PiApi } from './extension.ts';
import { renderers } from './render.ts';

export default function controlroom(pi: PiApi): void {
  activate(pi, process.env, renderers);
}
