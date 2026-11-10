import {
  commands,
  events,
  type AppInfo,
  type BarAction,
  type BarActionOutcome,
  type BarArtworkOutcome,
  type BarChanged,
  type BarSnapshotOutcome,
  type BarWeatherOutcome,
  type HostStatus,
  type SessionAction,
  type SessionActionOutcome,
  type ModuleStatusView,
  type ConfigFileView,
} from './bindings.gen';

export type {
  AppInfo,
  BarAction,
  BarActionOutcome,
  BarArtworkOutcome,
  BarChanged,
  BarSnapshotOutcome,
  BarWeatherOutcome,
  ConfigFileView,
  HostStatus,
  ModuleStatusView,
  SessionAction,
  SessionActionOutcome,
};

export const APP_INFO_QUERY_KEY = ['app', 'info'] as const;
export const HOST_STATUS_QUERY_KEY = ['host', 'status'] as const;
export const BAR_SNAPSHOT_QUERY_KEY = ['bar', 'snapshot'] as const;

/** Reads the static application identity from the host. */
export function readAppInfo(): Promise<AppInfo> {
  return commands.appInfo();
}

/** Reads the host's last published status snapshot. */
export function readHostStatus(): Promise<HostStatus> {
  return commands.hostStatus();
}

/**
 * Asks the host to request Accessibility. Resolves when the command is queued;
 * the result arrives through the status stream.
 */
export function requestAccessibility(): Promise<void> {
  return commands.requestAccessibility();
}

/** Asks the host to open the Accessibility pane of System Settings. */
export function openAccessibilitySettings(): Promise<void> {
  return commands.openAccessibilitySettings();
}

/** Asks the host to quit through its shared shutdown coordinator. */
export function quitApp(): Promise<void> {
  return commands.quitApp();
}

/**
 * Subscribes to host status events. The generated `events.statusChanged`
 * wrapper owns the event name; callers receive the unwrapped status.
 */
export function subscribeToStatus(onStatus: (status: HostStatus) => void): Promise<() => void> {
  return events.statusChanged.listen(({ payload }) => {
    onStatus(payload.status);
  });
}

/**
 * Reads the Bar view for the calling Bar window. The host authorizes the actual
 * webview label; any other window receives `unauthorized`.
 */
export function readBarSnapshot(): Promise<BarSnapshotOutcome> {
  return commands.barSnapshot();
}

/**
 * Reads the weather inputs (provider, key, location) for the calling Bar window.
 * The answer carries a secret and coordinates: callers keep it in function scope
 * and never in a query key, cache entry, store or log.
 */
export function readBarWeather(): Promise<BarWeatherOutcome> {
  return commands.barWeather();
}

/**
 * Asks the host to run one fixed Bar click action. The argument is a closed
 * enum value: the host maps it to its fixed target and accepts no name, path or
 * URL. The outcome is diagnostic only; `completed` means macOS reported the
 * request done, not that a window is visible.
 */
export function runBarAction(action: BarAction): Promise<BarActionOutcome> {
  return commands.barAction(action);
}

/**
 * Asks the host to pause or resume one module for this session. The action is a
 * closed enum (`pause` | `resume`); the host authorizes the calling window and
 * module, rechecks the current session state at execution and returns what it
 * actually did (`stale` when the state moved since the caller observed it).
 */
export function runSessionAction(
  module: string,
  action: SessionAction,
): Promise<SessionActionOutcome> {
  return commands.barSessionAction(module, action);
}

/**
 * Reads the 128x128 PNG (base64) for an `artworkId` of `BarView.media`. The id is a
 * non-secret content hash; the answer is typed (`ready`, `missing`, `unauthorized`,
 * `unavailable`).
 */
export function readBarMediaArtwork(id: string): Promise<BarArtworkOutcome> {
  return commands.barMediaArtwork(id);
}

/**
 * Subscribes to `bar-changed`. The generated `events.barChanged` wrapper owns the
 * event name; callers receive the whole payload (generation, sequence,
 * `resnapshot` and the optional outcome).
 */
export function subscribeToBar(onChanged: (changed: BarChanged) => void): Promise<() => void> {
  return events.barChanged.listen(({ payload }) => {
    onChanged(payload);
  });
}
