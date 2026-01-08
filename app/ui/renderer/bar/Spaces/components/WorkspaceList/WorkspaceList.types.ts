import type { Workspaces } from '../../Spaces.types';

export type WorkspaceListProps = {
  workspaces: Workspaces;
  focusedWorkspace: string | undefined;
  onSpaceClick: (name: string) => () => void;
};
