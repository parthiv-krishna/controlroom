// Schedules (spec §8.1) as text: the human form of a cron or interval spec, cron validation for the
// editor (the server has the final word), and run outcomes. Next and past run times come from the
// server.
import type { ScheduleOutcome, ScheduleRun, ScheduleSpec } from '../protocol';

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

const MONTHS = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
const DAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];

/** Replaces month or weekday names (any case) with their numbers. */
function numbered(src: string, names: string[], first: number): string {
  return src.toUpperCase().replace(/[A-Z]{3}/g, (n) => (names.includes(n) ? String(names.indexOf(n) + first) : n));
}

/**
 * Parses a five-field cron expression (minute hour day-of-month month day-of-week; month and
 * weekday names allowed). The server also accepts croner's extensions (`L`, `W`, `#`).
 */
export function parseCron(expr: string): Cron | null {
  const f = expr.trim().split(/\s+/);
  if (f.length !== 5) return null;
  f[3] = numbered(f[3], MONTHS, 1);
  f[4] = numbered(f[4], DAYS, 0);
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

/** "in 5m", "in 3h", "in 2d" (or "now") until `t`. */
export function inTime(t: number, now = Date.now()): string {
  const m = Math.round((t - now) / 60e3);
  if (m < 1) return 'now';
  if (m < 60) return `in ${m}m`;
  if (m < 48 * 60) return `in ${Math.round(m / 60)}h`;
  return `in ${Math.round(m / 1440)}d`;
}

const OUTCOME: Record<ScheduleOutcome, string> = { fired: 'fired', queued: 'queued (busy)', skipped: 'skipped', missed: 'missed', failed: 'failed', unknown: '?' };

/** A run's outcome, e.g. "missed ×3". */
export function runText(r: ScheduleRun): string {
  return `${OUTCOME[r.outcome]}${r.count > 1 ? ` ×${r.count}` : ''}`;
}

/** A run's scheduled time in the schedule's zone; a range for collapsed misses. */
export function runWhen(r: ScheduleRun, timeZone: string): string {
  return r.until == null ? formatIn(r.at, timeZone) : `${formatIn(r.at, timeZone)} – ${formatIn(r.until, timeZone)}`;
}
