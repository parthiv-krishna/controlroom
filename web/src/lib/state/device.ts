// This browser's device (spec §13 "Device names"): a persistent device_id in localStorage and a
// user-chosen label. Until the user names it (or skips), the session list shows the naming prompt.
import { guessDeviceName } from '../model/misc';

const ID = 'cr-device-id';
const LABEL = 'cr-device-label';
const NAMED = 'cr-device-named';

export interface LocalDevice {
  id: string;
  /** The guess from the user agent until the user names the device. */
  label: string;
  named: boolean;
}

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // storage disabled: the device gets a new id next time
  }
}

export function loadDevice(): LocalDevice {
  let id = read(ID);
  if (!id) write(ID, (id = crypto.randomUUID()));
  const guess = guessDeviceName(typeof navigator === 'undefined' ? '' : navigator.userAgent);
  return { id, label: read(LABEL) ?? guess, named: read(NAMED) === '1' };
}

/** Records the label the user chose (or kept, when skipping the prompt). */
export function rememberLabel(label: string): void {
  write(LABEL, label);
  write(NAMED, '1');
}
