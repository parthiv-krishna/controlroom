// Presentation of server-owned state: one icon language for sessions and machines (spec §13).
// Nothing here computes session state or attention; it only maps the summary's fields to icons
// and labels. A `stale` session (its machine is unreachable) shows as disconnected.
import type { Attention, Machine, MachineSession, SessionState } from '../protocol';

export type StatusKind = 'working' | 'attention' | 'asking' | 'complete' | 'done' | 'stopped' | 'idle' | 'offline' | 'archived';

/** The session's attention as the UI counts it: none while closed or disconnected. */
export function attentionOf(s: MachineSession): Exclude<Attention, 'unknown'> | null {
  if (s.stale || s.state === 'closed' || !s.attention || s.attention === 'unknown') return null;
  return s.attention;
}

/** A run is active or waiting on the user (the daemon reports `working` through queue hand-overs). */
export const busy = (s: { state: SessionState }): boolean =>
  s.state === 'working' || s.state === 'starting' || s.state === 'awaiting_permission' || s.state === 'awaiting_input';

const STOPPED_OUTCOMES = new Set(['cancelled', 'failed', 'max_tokens', 'refusal']);

export function statusKind(s: MachineSession, ignoreStale = false): StatusKind {
  if (s.state === 'closed') return 'archived';
  if (s.stale && !ignoreStale) return 'offline';
  switch (s.state) {
    case 'awaiting_permission':
    case 'awaiting_input':
      return 'attention';
    case 'working':
    case 'starting':
      return 'working';
    case 'failed':
    case 'interrupted':
      return 'stopped';
    default:
  }
  if (s.attention === 'question') return 'asking';
  if (s.state !== 'idle') return 'idle';
  if (s.last_outcome && STOPPED_OUTCOMES.has(s.last_outcome)) return 'stopped';
  if (s.last_outcome === 'completed') return s.unread ? 'complete' : 'done';
  return 'idle';
}

export function statusLabel(s: MachineSession, ignoreStale = false): string {
  switch (statusKind(s, ignoreStale)) {
    case 'archived': return 'Archived';
    case 'offline': return 'Disconnected';
    case 'attention': return s.state === 'awaiting_input' ? 'Needs answer' : 'Needs approval';
    case 'working': return s.state === 'starting' ? 'Starting' : 'Working';
    case 'asking': return 'Question';
    case 'stopped':
      if (s.state === 'failed') return 'Failed';
      if (s.state === 'interrupted') return 'Interrupted';
      return s.last_outcome === 'cancelled' ? 'Stopped' : `Stopped (${(s.last_outcome ?? '').replace(/_/g, ' ')})`;
    case 'complete': return 'Complete · unread';
    case 'done': return 'Complete';
    default: return s.state === 'dormant' ? 'Asleep' : 'Idle';
  }
}

/** The tooltip: a disconnected session names its machine and its last known state. */
export function statusTitle(s: MachineSession, m: Machine | undefined, now = Date.now()): string {
  if (statusKind(s) !== 'offline' || !m) return statusLabel(s);
  return `${m.name} ${connLabel(m)} · last seen ${ago(m.last_seen, now)} · was: ${statusLabel(s, true)}`;
}

// ---------------------------------------------------------------------------------------- machines

export const isLive = (m: Machine | undefined): boolean => m?.connectivity.state === 'connected';

/** A known daemon being replaced (connecting while installing or starting). */
export const upgrading = (m: Machine): boolean =>
  m.connectivity.state === 'connecting' && !!m.version && (m.bootstrap?.step === 'install' || m.bootstrap?.step === 'start');

/** Short labels of the reasons a machine needs the user. */
const ACTION_LABELS: Partial<Record<string, string>> = {
  auth_failed: 'key rejected',
  host_key_changed: 'host key changed',
  host_key_unconfirmed: 'confirm host key',
  controller_conflict: 'another server connected',
};

export function connLabel(m: Machine): string {
  const c = m.connectivity;
  switch (c.state) {
    case 'connected': return 'connected';
    case 'connecting': return upgrading(m) ? 'upgrading…' : m.bootstrap?.message ?? 'connecting…';
    case 'unreachable': return 'unreachable';
    case 'incompatible': return 'outdated';
    case 'action_required': return ACTION_LABELS[c.reason] ?? 'needs attention';
    default: return 'unknown';
  }
}

/** The machine's icon: the flag only when the user must act. */
export function connKind(m: Machine): StatusKind {
  switch (m.connectivity.state) {
    case 'connected': return 'complete';
    case 'unreachable': return 'offline';
    case 'incompatible':
    case 'action_required': return 'attention';
    default: return 'working';
  }
}

// ------------------------------------------------------------------------------------------- time

export function ago(t: number | null | undefined, now = Date.now()): string {
  if (t == null) return 'never';
  const d = now - t;
  if (d < 45e3) return 'just now';
  if (d < 3600e3) return `${Math.round(d / 60e3)}m ago`;
  if (d < 86400e3) return `${Math.round(d / 3600e3)}h ago`;
  return `${Math.round(d / 86400e3)}d ago`;
}

export function duration(ms: number): string {
  if (ms < 60e3) return `${Math.max(0, Math.round(ms / 1000))}s`;
  if (ms < 3600e3) return `${Math.round(ms / 60e3)}m`;
  return `${(ms / 3600e3).toFixed(1)}h`;
}
