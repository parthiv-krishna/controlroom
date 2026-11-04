// The system prompt for remote tools (design §5, spec §9), rebuilt from `session.info` before
// every run: the workspaces' instruction files as pi's project context (named `machine:path`,
// shared parent files once), the workspace skills, a `<workspaces>` section (machines, roots,
// shells and their directories), and a cwd line saying there is no local directory.

import type { SessionInfo } from '../protocol/SessionInfo.ts';
import type { WorkspaceInfo } from '../protocol/WorkspaceInfo.ts';
import { shown, workspaceDisplay } from './target.ts';

/** The `<workspaces>` section's tag. */
export const WORKSPACES_SECTION = 'workspaces';

/** pi's `<cwd>` section: pi itself runs on the server, in a scratch directory. */
export const CWD = 'none on this machine: pi runs on the controlroom server. Address files as machine:path (see <workspaces>).';

/** An instruction file in pi's `contextFiles` shape. */
export interface ContextFile {
  path: string;
  content: string;
}

/** A skill in pi's `Skill` shape (its prompt lists name, description and location). */
export interface PromptSkill {
  name: string;
  description: string;
  filePath: string;
  baseDir: string;
  sourceInfo: { path: string; source: string; scope: 'project'; origin: 'top-level'; baseDir: string };
  disableModelInvocation: boolean;
}

/** The workspaces' instruction files, outermost first, each once. */
export function contextFiles(info: SessionInfo): ContextFile[] {
  const seen = new Set<string>();
  const files: ContextFile[] = [];
  for (const w of info.workspaces) {
    for (const file of w.context_files) {
      const path = shown(info, w.machine, file.path);
      if (seen.has(path)) continue;
      seen.add(path);
      files.push({ path, content: file.content });
    }
  }
  return files;
}

/** The workspaces' skills, located as `machine:path`. */
export function skills(info: SessionInfo): PromptSkill[] {
  const seen = new Set<string>();
  const out: PromptSkill[] = [];
  for (const w of info.workspaces) {
    for (const skill of w.skills) {
      const filePath = shown(info, w.machine, skill.path);
      if (seen.has(filePath)) continue;
      seen.add(filePath);
      const baseDir = filePath.slice(0, filePath.lastIndexOf('/')) || filePath;
      out.push({
        name: skill.name,
        description: skill.description,
        filePath,
        baseDir,
        sourceInfo: { path: filePath, source: 'controlroom', scope: 'project', origin: 'top-level', baseDir },
        disableModelInvocation: false,
      });
    }
  }
  return out;
}

/** The `<workspaces>` section. */
export function workspacesSection(info: SessionInfo): string {
  const [first] = info.workspaces;
  if (!first) return 'This session has no workspaces yet: there are no files or shells to work with.';
  const primary = info.workspaces.find((w) => w.primary) ?? first;
  const paths =
    info.workspaces.length === 1
      ? `A path without a machine means ${first.machine}.`
      : 'This session has several workspaces, so always name the machine: a path without one is refused.';
  const lines = [
    `You work on remote machines. Every file path is machine:path, e.g. ${workspaceDisplay(first)}/README.md.`,
    `Relative paths resolve against that machine's workspace root; ~ is the remote home. ${paths}`,
    'Workspaces:',
    ...info.workspaces.map((w) => `- ${describeWorkspace(info, w)}`),
    `Shells (bash {shell}; default "${primary.shell}"): state (cwd, env, venv) persists per shell. Each bash result starts with [shell · cwd · exit].`,
    ...info.shells.map((s) => `- ${s.name}: on ${s.machine}, now in ${shown(info, s.machine, s.cwd)}`),
    'Use shell_new to open another shell, e.g. for parallel work or a long-running server.',
  ];
  return lines.join('\n');
}

function describeWorkspace(info: SessionInfo, w: WorkspaceInfo): string {
  const root = w.root ? shown(info, w.machine, w.root) : 'not opened yet';
  const parts = [`root ${root}`, `default shell "${w.shell}"`];
  if (w.primary && info.workspaces.length > 1) parts.unshift('primary');
  if (w.availability.connectivity.state !== 'connected') {
    parts.push(`${w.machine} unreachable since ${new Date(w.availability.since).toISOString()}; calls there wait for it`);
  }
  return `${workspaceDisplay(w)} (${parts.join('; ')})`;
}
