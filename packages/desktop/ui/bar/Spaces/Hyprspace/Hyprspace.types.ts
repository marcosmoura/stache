import type { IconSvgElement } from '@hugeicons/react';

export type HyprspaceWorkspacePayload = {
  workspace: string;
};

export type HyprspaceCurrentWorkspacePayload = HyprspaceWorkspacePayload;
export type HyprspaceWorkspacesPayload = HyprspaceWorkspacePayload[];

export type Workspace = {
  name: string;
  icon: IconSvgElement;
};

export type WorkspaceList = Workspace[];

export interface HyprspaceWindow {
  appName: string;
  windowId: number;
  windowTitle: string;
}

export type HyprspaceWindowsPayload = HyprspaceWindow[];
