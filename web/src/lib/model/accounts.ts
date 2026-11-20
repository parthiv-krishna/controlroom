// Named accounts of a model provider (Settings › Models): each account is a provider of its own
// for pi (e.g. `anthropic-work`), so models, the model picker and usage are per account. These
// helpers name accounts where a provider has several, and leave single-account providers as they
// were.
import type { AccountUsage, ModelAccount, ModelProvider, UsageTotals } from '../protocol';
import { formatTokens } from './statusline';

/** The accounts of providers with several, from Settings › Models' rows (as `models.accounts`). */
export function accountsFromProviders(providers: readonly ModelProvider[]): ModelAccount[] {
  const out: ModelAccount[] = [];
  const bases = [...new Set(providers.flatMap((p) => (p.account_of ? [p.account_of] : [])))].sort();
  for (const of of bases) {
    const first = providers.find((p) => p.id === of);
    out.push({ provider: of, of, name: first?.account ?? 'default' });
    for (const p of providers) if (p.account_of === of) out.push({ provider: p.id, of, name: p.account ?? p.id });
  }
  return out;
}

/** The account a pi provider id is, when its provider has several. */
export const accountName = (accounts: readonly ModelAccount[], provider: string | null): string | null =>
  (provider !== null && accounts.find((a) => a.provider === provider)?.name) || null;

/** The provider of a model option's value (`<provider>/<model id>`). */
export function modelProvider(value: string): string | null {
  const slash = value.indexOf('/');
  return slash > 0 ? value.slice(0, slash) : null;
}

/** A model choice's label, naming its account when its provider has several. */
export function modelLabel(accounts: readonly ModelAccount[], value: string, label: string): string {
  const name = accountName(accounts, modelProvider(value));
  return name ? `${label} · ${name}` : label;
}

/** Whether a Settings › Models row's provider has several accounts. */
export const hasAccounts = (p: ModelProvider, providers: readonly ModelProvider[]): boolean =>
  !!p.account_of || providers.some((q) => q.account_of === p.id);

/** A provider's name in labels: its label, with the account's name when it has several. */
export const providerName = (p: ModelProvider, providers: readonly ModelProvider[]): string =>
  hasAccounts(p, providers) ? `${p.label} (${p.account ?? 'default'})` : p.label;

const tokenCount = (t: UsageTotals): number => t.tokens.input + t.tokens.output + t.tokens.cache_read + t.tokens.cache_write;

/** One period's usage, tersely: `1.2M tok $0.420`. */
const totals = (t: UsageTotals): string => `${formatTokens(tokenCount(t))} tok${t.cost ? ` $${t.cost.toFixed(3)}` : ''}`;

/** An account's usage line: `24 h 1.2M tok $0.420 · 7 d … · all …`; null when it has none. */
export function usageLine(u: AccountUsage | null): string | null {
  if (!u || (u.total.cost === 0 && tokenCount(u.total) === 0)) return null;
  return `24 h ${totals(u.day)} · 7 d ${totals(u.week)} · all ${totals(u.total)}`;
}
