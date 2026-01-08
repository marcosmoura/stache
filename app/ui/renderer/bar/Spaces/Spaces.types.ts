export type HyprspaceWorkspacePayload = {
  workspace: string;
};

export type FocusedAppPayload = {
  appName: string;
  windowId: number;
  windowTitle: string;
}[];

export type Workspaces = {
  name: string;
  displayName: string;
}[];

export type WorkspaceWindows = {
  appName: string;
  windowId: number;
  windowTitle: string;
}[];
