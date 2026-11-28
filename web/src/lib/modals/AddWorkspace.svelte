<script lang="ts">
  // Another workspace for an existing session (its ⋯ menu): a machine of the session's profile and
  // a directory on it, picked as in the new-session dialog. The agent learns of it in its next
  // turn. A directory another open session uses is warned about; a refusal (not in the profile,
  // machine unreachable, bad path) shows inline. The directory list opens above its field.
  import { newOpId, useApp } from '../app/app.svelte';
  import { describeError, unreachableRefusal } from '../api/errors';
  import { displayName } from '../model/filters';
  import { connLabel, isLive } from '../model/status';
  import { sessionsAt } from '../model/workspaces';
  import type { FeedSession } from '../protocol';
  import Modal from '../ui/Modal.svelte';
  import WorkspacePicker from './WorkspacePicker.svelte';

  let { s }: { s: FeedSession } = $props();
  const app = useApp();
  const feed = app.client.feed;
  const close = () => (app.view.modal = null);

  /** The first connected machine of the profile the session doesn't use yet (else any connected one). */
  function firstMachine(): string {
    const live = app.ctxOf(s.profile_id).machineList.filter(isLive);
    return (live.find((m) => !s.workspaces.some((w) => w.machine_id === m.id)) ?? live[0])?.id ?? '';
  }
  let machine = $state(firstMachine());
  let path = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  const user = $derived(feed.machines.get(machine)?.user ?? '');
  const shared = $derived(sessionsAt(app.all.sessions.filter((o) => o.state !== 'closed' && o.id !== s.id), machine, path, user).map(displayName));

  /** The reason the workspace can't be added yet, if any. */
  function problem(): string | null {
    const m = feed.machines.get(machine);
    if (!m) return 'Pick a machine';
    if (!isLive(m)) return `${m.name} is ${connLabel(m)}: a workspace can't open there`;
    if (!path.trim()) return 'Enter a directory';
    if (sessionsAt([s], machine, path, user).length) return `Already a workspace of this session`;
    return null;
  }

  async function add() {
    if (busy) return;
    error = problem();
    if (error) return;
    busy = true;
    try {
      await app.client.request('session.workspace_add', { op_id: newOpId(), id: s.id, workspace: { machine_id: machine, path: path.trim() } });
      close();
    } catch (e) {
      error = unreachableRefusal(e, feed.machines) ?? describeError(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal label="Add workspace" short oncancel={close} onprimary={add}>
  <h3>Add workspace</h3>
  <div class="ws" role="group" aria-label="Workspace">
    <WorkspacePicker bind:machine bind:path profile={s.profile_id} autofocus above />
  </div>
  {#if shared.length}<div class="small warn-t" data-overlap>⚠ Also used by {shared.join(', ')}: edits can collide</div>{/if}
  {#if error}<div class="small bad-t" role="alert">{error}</div>{/if}
  <div class="acts">
    <button class="btn" onclick={close}><span class="kbd">Esc</span> Cancel</button>
    <button class="btn primary" disabled={busy || !app.client.online} onclick={add}><span class="kbd">↵</span> Add</button>
  </div>
</Modal>

<style>
  .ws { display: grid; grid-template-columns: minmax(110px, 0.35fr) auto minmax(0, 1fr); align-items: center; gap: 6px; color: var(--fg); }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  @media (max-width: 760px) { .ws { grid-template-columns: 1fr; } }
</style>
