// Every call has a client-side timeout (protocol §2.4): 30 s, longer for known slow methods.
import type { UiMethod } from '../protocol';

export const DEFAULT_TIMEOUT_MS = 30_000;

const SLOW: Partial<Record<UiMethod, number>> = {
  'machines.add': 60_000,
  'machines.add_confirm': 300_000,
  'machines.upgrade': 300_000,
  'doctor.report': 120_000,
  'fs.list': 60_000,
};

export const timeoutFor = (method: UiMethod): number => SLOW[method] ?? DEFAULT_TIMEOUT_MS;
