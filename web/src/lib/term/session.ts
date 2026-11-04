// What the terminal dock needs of a session: its shells and their workspaces (machine and
// directory), grouped the way the dock shows them.
import type { MachineId, SessionSummary, Shell, Workspace } from '../protocol';

export type DockSession = Pick<SessionSummary, 'id' | 'shells' | 'workspaces'>;

export interface ShellGroup {
  workspace: Workspace | null;
  /** `machine:path`, or the shell's own label when its workspace is unknown. */
  label: string;
  shells: Shell[];
}

/** Shells grouped by workspace, in workspace order (primary first), shells in creation order. */
export function shellGroups(s: DockSession, machineName: (id: MachineId) => string): ShellGroup[] {
  const groups: ShellGroup[] = s.workspaces.map((w) => ({ workspace: w, label: `${machineName(w.machine_id)}:${w.path}`, shells: [] }));
  const byId = new Map(groups.map((g) => [g.workspace?.id, g]));
  for (const shell of s.shells) {
    let group = byId.get(shell.workspace_id);
    if (!group) {
      group = { workspace: null, label: 'other', shells: [] };
      byId.set(shell.workspace_id, group);
      groups.push(group);
    }
    group.shells.push(shell);
  }
  return groups.filter((g) => g.shells.length);
}
