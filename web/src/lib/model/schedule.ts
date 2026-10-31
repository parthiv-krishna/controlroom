// Schedules (spec §8.1) as text: the human form of a cron or interval spec, and cron validation for
// the editor. Next and past run times come from the server.
import type { ScheduleSpec } from '../protocol';

const DOW = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const DOW_LONG = ['Sundays', 'Mondays', 'Tuesdays', 'Wednesdays', 'Thursdays', 'Fridays', 'Saturdays'];

/** One cron field's allowed values (`*`, `n`, `a-b`, `a,b`, `*\/n`, `a-b/n`), or null if invalid. */
function field(src: string, min: number, max: number): Set<number> | null {
  const out = new Set<number>();
  for (const part of src.split(',')) {
    const m = /^(\*|(\d+)(?:-(\d+))?)(?:\/(\d+))?$/.exec(part.trim());
    if (!m) return null;
    const step = m[4] ? Number(m[4]) : 1;
    const lo = m[1] === '*' ? min : Number(m[2]);
    const hi = m[1] === '*' ? max : m[3] ? Number(m[3]) : m[4] ? max : lo;
    if (lo < min || hi > max || lo > hi || step < 1) return null;
    for (let v = lo; v <= hi; v += step) out.add(v === 7 && max === 7 ? 0 : v);
  }
  return out;
}

interface Cron {
  dow: Set<number>;
  raw: string[];
}

/** Parses a five-field cron expression (minute hour day-of-month month day-of-week). */
export function parseCron(expr: string): Cron | null {
  const f = expr.trim().split(/\s+/);
  if (f.length !== 5) return null;
  const ok = [field(f[0], 0, 59), field(f[1], 0, 23), field(f[2], 1, 31), field(f[3], 1, 12)].every(Boolean);
  const dow = field(f[4], 0, 7);
  return ok && dow ? { dow, raw: f } : null;
}

const pad = (n: number) => String(n).padStart(2, '0');

function every(seconds: number): string {
  if (seconds % 86400 === 0) return seconds === 86400 ? 'Every day' : `Every ${seconds / 86400} days`;
  if (seconds % 3600 === 0) return seconds === 3600 ? 'Every hour' : `Every ${seconds / 3600} h`;
  return `Every ${Math.max(1, Math.round(seconds / 60))} min`;
}

/** "Weekdays 09:00", "Every 30 min", … (without the time zone). */
export function humanSchedule(spec: ScheduleSpec): string {
  if (spec.kind === 'interval') return every(spec.every_s);
  if (spec.kind !== 'cron') return 'Unknown schedule';
  const c = parseCron(spec.expr);
  if (!c) return `cron “${spec.expr}” (invalid)`;
  const [m, h, dom, mon, dow] = c.raw;
  if (/^\*\/\d+$/.test(m) && h === '*' && dom === '*' && mon === '*' && dow === '*') return `Every ${m.slice(2)} min`;
  if (/^\d+$/.test(m) && /^\*\/\d+$/.test(h) && dom === '*' && mon === '*' && dow === '*') return `Every ${h.slice(2)} h at :${pad(Number(m))}`;
  if (!/^\d+$/.test(m) || !/^\d+(,\d+)*$/.test(h) || mon !== '*') return `cron “${spec.expr}”`;
  const time = h.split(',').map((x) => `${pad(Number(x))}:${pad(Number(m))}`).join(', ');
  const days = [...c.dow].sort((a, b) => a - b);
  let when: string;
  if (dom !== '*' && dow === '*') when = `Monthly on day ${dom}`;
  else if (dow === '*') when = 'Daily';
  else if (days.join() === '1,2,3,4,5') when = 'Weekdays';
  else if (days.join() === '0,6') when = 'Weekends';
  else if (days.length === 1) when = DOW_LONG[days[0]];
  else when = days.map((d) => DOW[d]).join(', ');
  return `${when} ${time}`;
}

/** A time in the schedule's zone, e.g. "Mon 09:00". */
export function formatIn(t: number, timeZone: string): string {
  try {
    return new Intl.DateTimeFormat('en-US', { timeZone, weekday: 'short', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).format(new Date(t));
  } catch {
    return new Date(t).toISOString().slice(0, 16).replace('T', ' ');
  }
}

/** The browser's IANA zone, the editor's default. */
export function localZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return 'UTC';
  }
}
