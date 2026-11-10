// Settings › Models: a provider's status chip and detail line, the defaults pickers' options,
// and the gateway form's parsing.
import type { AuthHeader, LoginStep, ModelChoice, ModelProvider, StoredKey } from '../protocol';
import { ago } from './status';

export type Tone = 'ok' | 'bad' | 'warn' | 'muted';

/** A stored key is a pasted subscription token (e.g. Anthropic's `sk-ant-oat…`) rather than an API key. */
export const isToken = (key: StoredKey | null): boolean => key?.kind === 'subscription_token';

/** The status chip: ok · test failed · signed in · from server config · key/token set …4f2a · not signed in · no key. */
export function providerStatus(p: ModelProvider): { text: string; tone: Tone } {
  if (p.test && !p.test.ok) return { text: 'test failed', tone: 'bad' };
  if (p.test?.ok) return { text: 'ok', tone: 'ok' };
  if (p.signed_in) return { text: 'signed in', tone: 'ok' };
  if (p.source === 'server') return { text: 'from server config', tone: 'muted' };
  if (p.key) return { text: `${isToken(p.key) ? 'token' : 'key'} set …${p.key.last4}`, tone: 'ok' };
  if (p.can_sign_in && !p.takes_key) return { text: 'not signed in', tone: 'muted' };
  return { text: p.can_sign_in ? 'not set up' : 'no key', tone: 'muted' };
}

/** The detail line under a provider: where it comes from, the key, the account, the last test. */
export function providerDetail(p: ModelProvider, deviceLabel: (id: string, fallback: string) => string, now = Date.now()): string {
  const parts: string[] = [];
  if (p.source === 'server') parts.push('from server config');
  if (p.signed_in) parts.push(p.signed_in.account ? `signed in as ${p.signed_in.account}` : 'signed in');
  if (p.key) {
    const by = p.key.added_by ? ` by ${deviceLabel(p.key.added_by.device_id, p.key.added_by.label)}` : '';
    parts.push(`${isToken(p.key) ? 'subscription token' : 'key'} …${p.key.last4} · added ${ago(p.key.added_at, now)}${by}`);
  }
  if (p.gateway) parts.push(`${p.gateway.base_url} · ${p.gateway.models.join(', ')}`);
  if (p.test) parts.push(`${p.test.ok ? 'test ok' : 'test failed'}: ${p.test.message} · ${ago(p.test.at, now)}`);
  return parts.join(' · ');
}

/** Sign-in steps that end the flow. */
export const isFinal = (step: LoginStep): boolean => ['done', 'failed', 'cancelled', 'timed_out'].includes(step.kind);

/** pi's thinking levels. */
export const THINKING_LEVELS = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'] as const;

export const AUTH_HEADERS: { value: AuthHeader; label: string }[] = [
  { value: 'bearer', label: 'Authorization: Bearer' },
  { value: 'x_api_key', label: 'x-api-key' },
  { value: 'api_key', label: 'api-key' },
];

/** Model ids typed into the gateway form: separated by commas, spaces or lines. */
export const parseModels = (text: string): string[] => [...new Set(text.split(/[\s,]+/).filter(Boolean))];

/** The providers that have models, for the defaults' provider picker. */
export function defaultProviders(models: ModelChoice[], providers: ModelProvider[]): { value: string; label: string }[] {
  const ids = [...new Set(models.map((m) => m.provider))];
  return ids.map((id) => ({ value: id, label: providers.find((p) => p.id === id)?.label ?? id }));
}
