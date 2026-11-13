import { queryOptions, skipToken } from '@tanstack/react-query';

import { commands, events, type AppInfo, type HostStatus } from '@/ipc';

import { createFeed } from './feed';

/** Static application identity; fetched once per client and never refetched. */
export function appInfoOptions() {
  return queryOptions({
    queryKey: ['host', 'appInfo'],
    queryFn: (): Promise<AppInfo> => commands.appInfo(),
    staleTime: 'static',
  });
}

/** The host status, written only by {@link hostStatusFeed} (read it with `useQuery`). */
export function hostStatusOptions() {
  return queryOptions<HostStatus>({
    queryKey: ['host', 'status'],
    queryFn: skipToken,
  });
}

/** `host_status` + `status-changed`; no sequences, so the latest event wins. */
export const hostStatusFeed = createFeed({
  options: hostStatusOptions(),
  snapshot: (): Promise<HostStatus> => commands.hostStatus(),
  event: events.statusChanged,
  read: (payload) => ({ value: payload.status as HostStatus }),
});
