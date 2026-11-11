export type AppProps = {
  appName: string;
  displayName: string;
  /** The strip's opaque window key. */
  windowId: string;
  isFocused: boolean;
  onClick: (windowId: string) => void;
};
