// What a call may do across a dropped connection (spec §13, protocol §7):
//
// - reads (and previews: `dry_run: true`) change nothing, so they are sent again after a
//   reconnect;
// - commands carrying an `op_id` are idempotent on the server (a repeat returns the first
//   result), so they are sent again too;
// - anything else sent before the drop has an unknown outcome, so it fails (`lost`).
//
// Subscriptions and the handshake (`hello`, `devices.register`, the `*.subscribe` calls) belong
// to one connection: the sync engine makes them again on every connection, so they are never
// resent by the transport.
import type { UiMethod } from '../protocol';

/** Methods that only read. */
const READS = new Set<UiMethod>([
  'keys.list',
  'profiles.list',
  'notifications.preferences',
  'sandbox.defaults',
  'job.list',
  'job.logs',
  'agents.list',
  'settings.get',
  'models.list',
  'models.accounts',
  'models.login_wait',
  'fs.list',
  'doctor.report',
]);

/** Calls the server answers before `hello` (everything else waits for the handshake). */
export const HANDSHAKE = new Set<UiMethod>(['hello', 'ping']);

/** Whether `method` only reads (its failure is no user action failing). */
export const isRead = (method: UiMethod): boolean => READS.has(method);

/** Whether a call sent before a drop is sent again after the reconnect. */
export function resendable(method: UiMethod, params: unknown): boolean {
  if (READS.has(method)) return true;
  if (typeof params !== 'object' || params === null) return false;
  const p = params as { op_id?: unknown; dry_run?: unknown };
  return typeof p.op_id === 'string' || p.dry_run === true;
}
