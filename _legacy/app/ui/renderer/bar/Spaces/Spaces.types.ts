/**
 * Information about a workspace from the tiling manager.
 */
export type TilingWorkspace = {
  name: string;
  screenId: number;
  screenName: string;
  layout: string;
  isVisible: boolean;
  isFocused: boolean;
  windowCount: number;
  windowIds: number[];
};

/**
 * Information about a window from the tiling manager.
 */
export type TilingWindow = {
  id: number;
  pid: number;
  appId: string;
  appName: string;
  title: string;
  workspace: string;
  isFocused: boolean;
};

/**
 * Processed workspace data for UI rendering.
 */
type Workspace = {
  name: string;
  displayName: string;
};

export type Workspaces = Workspace[];

/**
 * Processed window data for UI rendering.
 */
type WorkspaceWindow = {
  appName: string;
  windowId: number;
  windowTitle: string;
};

export type WorkspaceWindows = WorkspaceWindow[];

/**
 * Return type of the useSpaces hook.
 */
type SpaceApp = {
  appName: string;
  windowId: number;
  windowTitle: string;
  displayName: string;
};

export type SpacesState = {
  apps: SpaceApp[];
  workspaces: Workspaces;
  focusedWorkspace: string | null | undefined;
  focusedApp: Omit<SpaceApp, 'displayName'> | null | undefined;
  onSpaceClick: (name: string) => void;
  onAppClick: (windowId: number) => void;
  isEnabled: boolean;
};
