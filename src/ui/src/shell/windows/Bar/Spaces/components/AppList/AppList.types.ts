export type App = {
  appName: string;
  windowId: string;
  windowTitle: string;
  displayName: string;
};

export type FocusedApp = Omit<App, 'displayName'> | null | undefined;

export type AppListProps = {
  apps: App[];
  focusedApp: FocusedApp;
  onAppClick: (windowId: string) => void;
};
