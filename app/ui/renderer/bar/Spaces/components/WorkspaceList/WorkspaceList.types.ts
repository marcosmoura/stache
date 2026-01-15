import type { Workspaces } from '../../Spaces.types';

export type WorkspaceListProps = {
  workspaces: Workspaces;
  focusedWorkspace: string | null | undefined;
  onSpaceClick: (name: string) => () => void;
};
