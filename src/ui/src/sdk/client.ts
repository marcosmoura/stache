import {
  commands,
  events,
  type AppInfo,
  type HostStatus,
  type ModuleStatusView,
  type ConfigFileView,
} from './bindings.gen';

export type { AppInfo, ConfigFileView, HostStatus, ModuleStatusView };

export const APP_INFO_QUERY_KEY = ['app', 'info'] as const;
export const HOST_STATUS_QUERY_KEY = ['host', 'status'] as const;

/** Reads the static application identity from the host. */
export function readAppInfo(): Promise<AppInfo> {
  return commands.appInfo();
}

/** Reads the host's last published status snapshot. */
export function readHostStatus(): Promise<HostStatus> {
  return commands.hostStatus();
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
