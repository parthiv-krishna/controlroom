import { describe, expect, it } from 'vitest';
import type { ModelProvider } from '../protocol';
import { defaultProviders, isFinal, parseModels, providerDetail, providerHistory, providerStatus } from './models';

const base: ModelProvider = {
  id: 'anthropic',
  label: 'Anthropic',
  source: 'none',
  takes_key: true,
  token_prefix: 'sk-ant-oat',
  can_sign_in: true,
  key: null,
  signed_in: null,
  gateway: null,
  test: null,
  clash: null,
  account: null,
  account_of: null,
  can_add_account: false,
  usage: null,
};
const NOW = 1_767_600_000_000;

describe('providerStatus', () => {
  it('names the state, a failed test first', () => {
    expect(providerStatus(base).text).toBe('not set up');
    expect(providerStatus({ ...base, takes_key: false }).text).toBe('not signed in');
    expect(providerStatus({ ...base, can_sign_in: false }).text).toBe('no key');
    const key = { kind: 'api_key', last4: '4f2a', added_at: NOW, added_by: null } as const;
    expect(providerStatus({ ...base, source: 'ui', key })).toEqual({ text: 'key set …4f2a', tone: 'ok' });
    const token = { ...key, kind: 'subscription_token', last4: 'abcd' } as const;
    expect(providerStatus({ ...base, source: 'ui', key: token })).toEqual({ text: 'token set …abcd', tone: 'ok' });
    expect(providerStatus({ ...base, source: 'server' }).text).toBe('from server config');
    expect(providerStatus({ ...base, signed_in: { account: null } }).text).toBe('signed in');
    expect(providerStatus({ ...base, key, test: { ok: false, message: 'HTTP 401', at: NOW } })).toEqual({ text: 'test failed', tone: 'bad' });
    expect(providerStatus({ ...base, key, test: { ok: true, message: 'HTTP 200 OK', at: NOW } }).text).toBe('ok');
  });

  it('details only what the chip doesn\'t say; the history goes to the tooltip', () => {
    const p: ModelProvider = {
      ...base,
      source: 'ui',
      key: { kind: 'api_key', last4: '4f2a', added_at: NOW - 3_600_000, added_by: { device_id: 'd1', label: 'old label' } },
      test: { ok: false, message: 'HTTP 401 Unauthorized', at: NOW },
    };
    const label = (id: string, fallback: string) => (id === 'd1' ? 'Laptop' : fallback);
    expect(providerDetail(p)).toBe('HTTP 401 Unauthorized');
    expect(providerHistory(p, label, NOW)).toBe('Key …4f2a added 1h ago by Laptop\nTest failed just now: HTTP 401 Unauthorized');
    expect(providerDetail({ ...base, signed_in: { account: 'alice@example.com' } })).toBe('as alice@example.com');
    const token = { kind: 'subscription_token', last4: 'abcd', added_at: NOW - 3_600_000, added_by: null } as const;
    expect(providerDetail({ ...base, source: 'ui', key: token })).toBe('');
    expect(providerHistory({ ...base, source: 'ui', key: token }, label, NOW)).toBe('Subscription token …abcd added 1h ago');
  });
});

describe('helpers', () => {
  it('parses model lists and picks providers with models', () => {
    expect(parseModels(' a, b\nc  a ')).toEqual(['a', 'b', 'c']);
    expect(defaultProviders([{ provider: 'gw', id: 'm', name: 'M', reasoning: false }], [base])).toEqual([{ value: 'gw', label: 'gw' }]);
    expect(isFinal({ kind: 'done', account: null })).toBe(true);
    expect(isFinal({ kind: 'progress', message: '' })).toBe(false);
  });
});
