import { describe, expect, it } from 'vitest';
import type { AccountUsage, ModelProvider } from '../protocol';
import { accountName, accountsFromProviders, modelLabel, modelProvider, providerName, usageLine } from './accounts';

const row = (id: string, account: string | null, account_of: string | null = null): ModelProvider => ({
  id,
  label: 'Anthropic',
  source: 'ui',
  takes_key: true,
  token_prefix: null,
  can_sign_in: true,
  key: null,
  signed_in: null,
  gateway: null,
  test: null,
  clash: null,
  account,
  account_of,
  can_add_account: !account_of,
  usage: null,
});

describe('accounts', () => {
  const rows = [row('anthropic', 'personal'), row('anthropic-work', 'work', 'anthropic'), row('openai', 'default')];
  const accounts = accountsFromProviders(rows);

  it('lists the accounts of providers with several, the first one first', () => {
    expect(accounts).toEqual([
      { provider: 'anthropic', of: 'anthropic', name: 'personal' },
      { provider: 'anthropic-work', of: 'anthropic', name: 'work' },
    ]);
    expect(accountName(accounts, 'openai')).toBeNull();
    expect(accountName(accounts, null)).toBeNull();
  });

  it('labels models and providers by account only where there are several', () => {
    expect(modelProvider('anthropic-work/m1')).toBe('anthropic-work');
    expect(modelProvider('large')).toBeNull();
    expect(modelLabel(accounts, 'anthropic-work/m1', 'Model One')).toBe('Model One · work');
    expect(modelLabel(accounts, 'anthropic/m1', 'Model One')).toBe('Model One · personal');
    expect(modelLabel(accounts, 'openai/m2', 'Model Two')).toBe('Model Two');
    expect(providerName(rows[1], rows)).toBe('Anthropic (work)');
    expect(providerName(rows[0], rows)).toBe('Anthropic (personal)');
    expect(providerName(rows[2], rows)).toBe('Anthropic');
  });

  it('sums usage tersely', () => {
    const t = (n: number, cost: number) => ({ tokens: { input: n, output: n, cache_read: 0, cache_write: 0 }, cost });
    const usage: AccountUsage = { day: t(500, 0.1), week: t(6_000, 1.25), total: t(600_000, 0) };
    expect(usageLine(usage)).toBe('24 h 1.0k tok $0.100 · 7 d 12k tok $1.250 · all 1.2M tok');
    expect(usageLine({ day: t(0, 0), week: t(0, 0), total: t(0, 0) })).toBeNull();
    expect(usageLine(null)).toBeNull();
  });
});
