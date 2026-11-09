// Every call has a client-side timeout (protocol §2.4): 30 s, longer for known slow methods.
import type { UiMethod } from '../protocol';

const DEFAULT_TIMEOUT_MS = 30_000;

const SLOW: Partial<Record<UiMethod, number>> = {
  'machines.add': 60_000,
  'machines.add_confirm': 300_000,
  'machines.upgrade': 300_000,
  'doctor.report': 120_000,
  'fs.list': 60_000,
  // A sign-in's long poll answers within 20 s; a test or sign-in may wait for pi or the provider.
  'models.login_wait': 40_000,
  'models.login_start': 120_000,
  'models.sign_out': 120_000,
  'models.test': 120_000,
};

export const timeoutFor = (method: UiMethod): number => SLOW[method] ?? DEFAULT_TIMEOUT_MS;
