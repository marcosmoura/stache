import { useCallback, useMemo } from 'react';

import {
  ComputerTerminal01Icon,
  SourceCodeIcon,
  AiBrowserIcon,
  SpotifyIcon,
  FigmaIcon,
  SlackIcon,
  DashboardCircleIcon,
  AppleFinderIcon,
  Mail02Icon,
  AppleReminderIcon,
  AppStoreIcon,
  BrowserIcon,
  DiscordIcon,
  HardDriveIcon,
  Mail01Icon,
  SecurityPasswordIcon,
  SourceCodeCircleIcon,
  UserMultiple02Icon,
  VisualStudioCodeIcon,
  WhatsappIcon,
  ZoomIcon,
} from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';
import { cx } from '@linaria/core';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { useTauriEvent } from '@/hooks';
import { TilingEvents } from '@/types';

import { fetchWorkspaceList, onWorkspaceChange, onWorkspaceClick } from './Spaces.service';
import * as styles from './Spaces.styles';
import type { Workspaces } from './Spaces.types';

const workspaceIcons: Record<string, IconSvgElement> = {
  terminal: ComputerTerminal01Icon,
  coding: SourceCodeIcon,
  browser: AiBrowserIcon,
  music: SpotifyIcon,
  design: FigmaIcon,
  communication: SlackIcon,
  misc: DashboardCircleIcon,
  files: AppleFinderIcon,
  mail: Mail02Icon,
  tasks: AppleReminderIcon,
};

const appIcons = {
  'App Store': AppStoreIcon,
  'Microsoft Edge Dev': BrowserIcon,
  'Microsoft Outlook': Mail01Icon,
  'Microsoft Teams': UserMultiple02Icon,
  'Proton Drive': HardDriveIcon,
  'Proton Pass': SecurityPasswordIcon,
  'Zed Preview': SourceCodeCircleIcon,
  Code: VisualStudioCodeIcon,
  Discord: DiscordIcon,
  Figma: FigmaIcon,
  Finder: AppleFinderIcon,
  Ghostty: ComputerTerminal01Icon,
  Reminders: AppleReminderIcon,
  Slack: SlackIcon,
  Spotify: SpotifyIcon,
  WhatsApp: WhatsappIcon,
  // WTF? There is a special character in the app name
  '‎WhatsApp': WhatsappIcon,
  Zoom: ZoomIcon,
} as const;

const getAppIcon = (name: string) => {
  const appName = name.trim() as keyof typeof appIcons;

  return appIcons[appName] || DashboardCircleIcon;
};

export const Spaces = () => {
  const queryClient = useQueryClient();

  const { data: workspaces } = useQuery<Workspaces>({
    queryKey: ['workspaces'],
    queryFn: fetchWorkspaceList,
    refetchOnMount: true,
  });

  useTauriEvent<Workspaces>(TilingEvents.WORKSPACES_CHANGED, ({ payload }) => {
    onWorkspaceChange(payload, queryClient);
  });

  const focusedApp = useMemo(() => {
    return workspaces?.find((workspace) => workspace.isFocused)?.focusedApp;
  }, [workspaces]);

  const onSpaceClick = useCallback((name: string) => () => onWorkspaceClick(name), []);

  if (!workspaces) {
    return null;
  }

  return (
    <div className={styles.spaces} data-test-id="spaces-container">
      <Surface className={styles.workspaces}>
        {workspaces.map(({ name, isFocused }) => {
          const capitalizedName = name.charAt(0).toUpperCase() + name.slice(1);

          return (
            <Button
              key={name}
              className={cx(styles.workspace, isFocused && styles.workspaceActive)}
              active={isFocused}
              onClick={onSpaceClick(name)}
            >
              <Icon icon={workspaceIcons[name]} />
              {isFocused && <span>{capitalizedName}</span>}
            </Button>
          );
        })}
      </Surface>

      {focusedApp && (
        <Surface className={styles.app}>
          <Icon icon={getAppIcon(focusedApp.name)} />
          <span>{focusedApp.name}</span>
        </Surface>
      )}
    </div>
  );
};
