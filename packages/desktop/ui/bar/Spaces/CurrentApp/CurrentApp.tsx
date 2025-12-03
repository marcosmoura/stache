import { useQuery, useQueryClient } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { useTauriEvent } from '@/hooks';
import type { CLIEventPayload } from '@/types';

import { fetchCurrentHyprspaceWindow, getAppIcon, onCLIEvent } from './CurrentApp.service';
import * as styles from './CurrentApp.styles';

export const CurrentApp = () => {
  const queryClient = useQueryClient();
  const { data: focusedWindow } = useQuery({
    queryKey: ['hyprspace_focused_window'],
    queryFn: fetchCurrentHyprspaceWindow,
    refetchOnMount: true,
  });

  useTauriEvent<CLIEventPayload>('tauri_cli_event', ({ payload }) =>
    onCLIEvent(payload, queryClient),
  );

  if (!focusedWindow || focusedWindow.length === 0) {
    return null;
  }

  const app = focusedWindow[0].appName;

  return (
    <Surface as="div" className={styles.app}>
      <Icon icon={getAppIcon(app)} />
      <span>{app}</span>
    </Surface>
  );
};
