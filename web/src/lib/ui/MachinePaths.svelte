<script lang="ts">
  // Text with the `machine:path`s it names (known machines only) marked as remote paths, so they
  // read as files on that machine and can later open in a viewer (`data-machine`, `data-path`).
  import { useApp } from '../app/app.svelte';
  import { splitMachinePaths } from '../model/workspaces';

  let { text }: { text: string } = $props();
  const app = useApp();
  const runs = $derived(splitMachinePaths(text, app.client.feed.machineList.map((m) => m.name)));
</script>

{#each runs as r, i (i)}{#if r.machine}<span class="mpath" data-machine={r.machine} data-path={r.path}><b>{r.machine}</b>:{r.path}</span>{:else}{r.text}{/if}{/each}

<style>
  .mpath b { font-weight: 600; }
</style>
