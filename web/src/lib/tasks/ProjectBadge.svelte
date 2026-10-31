<script lang="ts">
  // A task's first project as a colored two-letter square ("+N" in the tooltip for more). Blank
  // but the same width without a project, so the priorities line up.
  import { useApp } from '../app/app.svelte';
  import { projectCode } from '../model/projects';
  import type { Project, Task } from '../protocol';

  let { t }: { t: Task } = $props();
  const app = useApp();
  const projects = $derived(t.project_ids.map((id) => app.client.feed.projects.get(id)).filter((p): p is Project => !!p));
  const title = $derived(projects.length ? projects.map((p) => p.name).join(', ') : 'No project');
</script>

{#if projects.length}
  <span class="pb" style:--pc={projects[0].color} {title} aria-label="Projects: {title}">{projectCode(projects[0])}{#if projects.length > 1}<sup>+</sup>{/if}</span>
{:else}
  <span class="pb none" aria-hidden="true"></span>
{/if}

<style>
  .pb { flex: none; width: 20px; height: 16px; border-radius: 4px; display: inline-grid; place-items: center; font: 650 9.5px var(--font); text-transform: capitalize; position: relative; background: color-mix(in srgb, var(--pc) 18%, transparent); color: color-mix(in srgb, var(--pc) 85%, var(--fg)); margin-top: 1px; }
  .pb.none { background: none; }
  sup { position: absolute; right: -5px; top: -6px; font-size: 9px; color: var(--muted); }
</style>
