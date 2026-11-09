<script lang="ts">
  // Settings: a large fixed-size modal, or a full page via the expand button (the choice persists).
  // Esc closes (or steps back in the add-machine flow); Enter runs the flow's primary action.
  import { useApp } from '../app/app.svelte';
  import type { SettingsTab } from '../app/view.svelte';
  import Icon from '../ui/Icon.svelte';
  import Modal from '../ui/Modal.svelte';
  import type AddMachine from './AddMachine.svelte';
  import DevicesTab from './DevicesTab.svelte';
  import KeysTab from './KeysTab.svelte';
  import MachinesTab from './MachinesTab.svelte';
  import ModelsTab from './ModelsTab.svelte';
  import NotificationsTab from './NotificationsTab.svelte';
  import OtherTabs from './OtherTabs.svelte';
  import SandboxTab from './SandboxTab.svelte';

  let { page }: { page: boolean } = $props();
  const app = useApp();
  const TABS: [SettingsTab, string][] = [
    ['machines', 'Machines'], ['keys', 'SSH keys'], ['models', 'Models'], ['notifications', 'Notifications'], ['devices', 'Devices'], ['sandbox', 'Sandbox'],
    ['contacts', 'Contacts'], ['schedules', 'Schedules'], ['keyboard', 'Keyboard'], ['appearance', 'Appearance'],
  ];
  const tab = $derived(app.view.settings ?? 'machines');
  let adding = $state(false);
  let wizard = $state<AddMachine>();

  const close = () => {
    adding = false;
    app.view.settings = null;
  };
  const cancel = () => (adding && wizard ? wizard.back() : close());
  const primary = () => wizard?.primary();
</script>

<Modal label="Settings" wide {page} oncancel={cancel} onprimary={adding ? primary : undefined}>
  <div class="sh">
    <h3>Settings</h3><span class="sp"></span>
    {#if app.desk}
      <button class="ic" title={app.prefs.settingsPage ? 'Show as dialog' : 'Expand to full page'} aria-label={app.prefs.settingsPage ? 'Show as dialog' : 'Expand to full page'} onclick={() => (app.prefs.settingsPage = !app.prefs.settingsPage)}><Icon name={app.prefs.settingsPage ? 'collapse' : 'expand'} /></button>
    {/if}
    <button class="ic" title="Close (Esc)" aria-label="Close settings" onclick={close}><Icon name="close" /></button>
  </div>
  <div class="tabs" role="tablist">
    {#each TABS as [t, label] (t)}
      <button class="tab" class:on={tab === t} role="tab" aria-selected={tab === t} onclick={() => { app.view.settings = t; adding = false; }}>{label}</button>
    {/each}
  </div>
  <div class="body" role="tabpanel">
    {#if tab === 'machines'}<MachinesTab bind:adding bind:wizard />
    {:else if tab === 'keys'}<KeysTab />
    {:else if tab === 'models'}<ModelsTab />
    {:else if tab === 'notifications'}<NotificationsTab />
    {:else if tab === 'devices'}<DevicesTab />
    {:else if tab === 'sandbox'}<SandboxTab />
    {:else}<OtherTabs {tab} />{/if}
  </div>
</Modal>

<style>
  .sh { display: flex; align-items: center; gap: 4px; }
  .ic { background: none; border: none; color: var(--muted); width: 30px; height: 30px; border-radius: 7px; display: grid; place-items: center; cursor: pointer; }
  .ic:hover { background: var(--bg3); color: var(--fg); }
  .tabs { display: flex; gap: 4px; border-bottom: 1px solid var(--line); overflow-x: auto; flex: none; }
  .tab { background: none; border: none; color: var(--muted); padding: 6px 12px; cursor: pointer; font: inherit; border-bottom: 2px solid transparent; white-space: nowrap; }
  .tab.on { color: var(--fg); border-bottom-color: var(--accent); }
  .body { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; padding: 4px 2px 40px; }
</style>
