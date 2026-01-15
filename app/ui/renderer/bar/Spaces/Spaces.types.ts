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
export type Workspaces = {
  name: string;
  displayName: string;
}[];

/**
 * Processed window data for UI rendering.
 */
export type WorkspaceWindows = {
  appName: string;
  windowId: number;
  windowTitle: string;
}[];
