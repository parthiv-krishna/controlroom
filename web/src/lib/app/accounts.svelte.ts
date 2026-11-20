// The accounts of model providers with several (Settings › Models), for naming the account of a
// session's model in its header. Loaded once when first needed (`models.accounts` is cheap) and
// kept current by Settings › Models, which sees every change.
import { accountsFromProviders } from '../model/accounts';
import type { ModelAccount, ModelProvider } from '../protocol';
import type { Client } from '../state/client.svelte';

export class Accounts {
  list = $state<ModelAccount[]>([]);
  private loaded = false;

  constructor(private readonly client: Client) {}

  /** Loads the accounts once the client is online (call from an effect). */
  ensure(): void {
    if (this.loaded || !this.client.online) return;
    this.loaded = true;
    this.client
      .request('models.accounts', {})
      .then((r) => (this.list = r.accounts))
      .catch(() => (this.loaded = false));
  }

  /** Takes Settings › Models' rows as the current accounts. */
  update(providers: readonly ModelProvider[]): void {
    this.loaded = true;
    this.list = accountsFromProviders(providers);
  }
}
