/**
 * Processed workspace data for UI rendering.
 */
type Workspace = {
  name: string;
  displayName: string;
};

export type Workspaces = Workspace[];

/**
 * Processed window data for UI rendering. `windowId` is the strip's opaque
 * window key (legacy used the numeric window id; never displayed).
 */
export type WorkspaceWindow = {
  appName: string;
  windowId: string;
  windowTitle: string;
};

export type WorkspaceWindows = WorkspaceWindow[];

/** The Tiling strip as Bar data, projected from `BarView.tiling` (`null` until resolved). */
export type SpacesStrip =
  | { kind: 'absent' }
  | { kind: 'loading' }
  | {
      kind: 'available';
      /** The rendered Tiling generation; a click echoes it back. */
      generation: string;
      /** The rendered Tiling sequence; a click echoes it back. */
      sequence: string;
      /** Sorted in legacy's workspace order. */
      workspaces: Workspaces;
      focusedWorkspace: string | null;
      /** The focused workspace's visible apps, in Tiling's order. */
      apps: WorkspaceWindows;
      focusedApp: WorkspaceWindow | null;
    };

/**
 * Return type of the useSpaces hook.
 */
type SpaceApp = WorkspaceWindow & {
  displayName: string;
};

export type SpacesState = {
  apps: SpaceApp[];
  workspaces: Workspaces;
  focusedWorkspace: string | null | undefined;
  focusedApp: WorkspaceWindow | null | undefined;
  onSpaceClick: (name: string) => void;
  onAppClick: (windowId: string) => void;
  /** Legacy's `isEnabled`: an `available` strip. */
  isEnabled: boolean;
  /** Tiling runs but has not published yet: legacy's suspended fallback. */
  isLoading: boolean;
};
