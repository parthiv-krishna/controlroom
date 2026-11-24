<script lang="ts">
  // The profile picker at the top of Settings › Machines, Models and Sandbox: one pill per profile
  // (its color, a lock when private), ＋ a new profile, and the picked profile's ⋯ (rename, color,
  // private, move out of it, delete). On a fresh install only ＋ shows.
  import { useApp } from '../app/app.svelte';
  import { archivedIn, deleteBlocker, profileLabel } from '../model/profiles';
  import type { Profile } from '../protocol';
  import ActionMenu, { type MenuAction } from '../ui/ActionMenu.svelte';
  import Icon from '../ui/Icon.svelte';

  const app = useApp();
  const online = $derived(app.client.online);
  const current = $derived(app.settingsProfile);

  const pick = (p: Profile) => (app.view.settingsProfile = p.id);
  const create = () => (app.view.modal = { kind: 'profile', id: null, then: (p) => (app.view.settingsProfile = p.id) });

  async function rename(p: Profile) {
    const name = await app.dialogs.ask({ title: `Rename ${p.name}`, value: p.name, ok: 'Rename' });
    if (name && name !== p.name) await app.client.call('profiles.update', { profile_id: p.id, name });
  }

  /** Private needs a listed account policy first (a private profile is never "Any"). */
  async function setPrivate(p: Profile) {
    if (p.private) {
      await app.client.call('profiles.update', { profile_id: p.id, private: false });
      return;
    }
    const body = 'Push names only the profile. Its machines can’t be shared.';
    if (await app.dialogs.confirm({ title: `Make ${p.name} private?`, body, ok: 'Make private' })) await app.client.call('profiles.update', { profile_id: p.id, private: true });
  }

  async function remove(p: Profile) {
    const archived = archivedIn(app.all.sessions, p.id);
    const body = archived ? `Its ${archived} archived session${archived === 1 ? ' is' : 's are'} deleted with it.` : 'Its sandbox defaults and model accounts go with it.';
    if (!(await app.dialogs.confirm({ title: `Delete ${p.name}?`, body, ok: 'Delete', danger: true }))) return;
    if ((await app.client.call('profiles.delete', { profile_id: p.id, delete_archived: archived > 0 })) === undefined) return;
    app.view.settingsProfile = null;
    if (app.view.profile === p.id) app.view.profile = null;
  }

  const actions = (p: Profile): MenuAction[] => {
    const blocker = deleteBlocker(app.all, p.id);
    return [
      { label: 'Rename…', disabled: !online, run: () => void rename(p) },
      { label: 'Color…', disabled: !online, run: () => (app.view.modal = { kind: 'profile', id: p.id }) },
      {
        label: 'Private',
        checked: p.private,
        disabled: !online || (!p.private && p.models.kind === 'any'),
        hint: !p.private && p.models.kind === 'any' ? `List the accounts in ${p.name} first (Models)` : 'Push names only the profile; no shared machines; accounts listed one by one',
        run: () => void setPrivate(p),
      },
      ...(app.multi ? [{ label: 'Move…', aria: `Move machines, projects or sessions from ${p.name}`, disabled: !online, run: () => app.moveToProfile({ from: p.id }) }] : []),
      { label: 'Delete…', danger: true, disabled: !online || !!blocker, hint: blocker ?? undefined, run: () => void remove(p) },
    ];
  };
</script>

<div class="pp">
  <div class="pills" role="radiogroup" aria-label="Profile">
    {#each app.profiles as p (p.id)}
      <button class="pill" class:on={p.id === current?.id} style:--pf={p.color} role="radio" aria-checked={p.id === current?.id} aria-label={profileLabel(p)} title={profileLabel(p)} onclick={() => pick(p)}>
        <span class="dot" aria-hidden="true"></span><span class="nm">{p.name}</span>{#if p.private}<Icon name="lock" size={11} label="private" />{/if}
      </button>
    {/each}
  </div>
  <button class="pill add" title="New profile" aria-label="New profile" disabled={!online} onclick={create}><Icon name="plus" size={13} /></button>
  <span class="sp"></span>
  {#if current}<ActionMenu label="Actions of profile {current.name}" actions={actions(current)} />{/if}
</div>

<style>
  .pp { display: flex; align-items: center; gap: 6px; padding: 2px 0 4px; flex: none; min-width: 0; }
  .pills { display: flex; gap: 6px; flex-wrap: wrap; min-width: 0; }
  .pill {
    display: inline-flex; align-items: center; gap: 6px; font: inherit; font-size: 13px; padding: 4px 11px 4px 9px; border-radius: 999px;
    border: 1px solid var(--line); background: none; color: var(--muted); cursor: pointer; max-width: 260px; min-width: 0;
  }
  .pill:hover { color: var(--fg); }
  .pill.on { border-color: var(--pf); background: color-mix(in srgb, var(--pf) 14%, transparent); color: var(--fg); font-weight: 600; }
  .pill.add { padding: 4px 8px; border-style: dashed; flex: none; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--pf); flex: none; }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
