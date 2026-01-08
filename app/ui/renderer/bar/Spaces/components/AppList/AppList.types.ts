export type App = {
  appName: string;
  windowId: number;
  windowTitle: string;
  displayName: string;
};

export type FocusedApp = Omit<App, 'displayName'> | undefined;

export type AppListProps = {
  apps: App[];
  focusedApp: FocusedApp;
  onAppClick: (windowId: number) => () => void;
};
