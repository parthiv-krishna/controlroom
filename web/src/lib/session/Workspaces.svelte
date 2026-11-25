<script lang="ts">
  // A session's workspaces in its header: the only one as a `machine:path` pill (with its
  // availability and the overlap ⚠), several as one collapsed chip whose popover lists them.
  import { useApp } from '../app/app.svelte';
  import { sharedWith } from '../model/workspaces';
  import type { FeedSession, Workspace } from '../protocol';
  import WorkspacePill from './WorkspacePill.svelte';
  import WorkspacesChip from './WorkspacesChip.svelte';

  let { s }: { s: FeedSession } = $props();
  const feed = useApp().client.feed;
  /** The other open sessions sharing a workspace's directory, if any. */
  const sharing = (w: Workspace): FeedSession[] => sharedWith(s, w, feed.overlaps, feed.sessions, feed.machines);
</script>

<span class="wss" aria-label="Workspaces">
  {#if s.workspaces.length === 1}
    {@const w = s.workspaces[0]}
    <WorkspacePill {w} others={sharing(w)} />
  {:else if s.workspaces.length > 1}
    <WorkspacesChip {s} {sharing} />
  {/if}
</span>

<style>
  .wss { display: inline-flex; align-items: center; gap: 2px; min-width: 0; max-width: 100%; }
</style>
