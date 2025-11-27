export interface HyprspaceWindow {
  appName: string;
  windowId: number;
  windowTitle: string;
}

export type HyprspaceWindowsPayload = HyprspaceWindow[];
