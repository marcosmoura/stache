import {
  AiBrowserIcon,
  Analytics01Icon,
  AppleFinderIcon,
  AppleReminderIcon,
  AppStoreIcon,
  ArcBrowserIcon,
  BrowserIcon,
  CheckListIcon,
  CodeSimpleIcon,
  CodeSquareIcon,
  ComputerTerminal01Icon,
  DashboardCircleIcon,
  DiscordIcon,
  FigmaIcon,
  Folder01Icon,
  HardDriveIcon,
  Mail01Icon,
  MailAtSign01Icon,
  MessageMultiple01Icon,
  MusicNote02Icon,
  MusicNote03Icon,
  SafariIcon,
  SecurityPasswordIcon,
  Settings01Icon,
  SlackIcon,
  SpotifyIcon,
  UserMultiple02Icon,
  VisualStudioCodeIcon,
  WhatsappIcon,
  ZoomIcon,
} from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';
import { cx } from '@linaria/core';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Stack } from '@/components/Stack';
import { Surface } from '@/components/Surface';

import { useSpaces } from './Spaces.state';
import * as styles from './Spaces.styles';

const workspaceIcons: Record<string, IconSvgElement> = {
  terminal: ComputerTerminal01Icon,
  coding: CodeSimpleIcon,
  browser: AiBrowserIcon,
  music: MusicNote03Icon,
  design: FigmaIcon,
  communication: MessageMultiple01Icon,
  misc: DashboardCircleIcon,
  files: Folder01Icon,
  mail: MailAtSign01Icon,
  tasks: CheckListIcon,
};

const appIcons = {
  'Activity Monitor': Analytics01Icon,
  'App Store': AppStoreIcon,
  'Archetype Gojira X': MusicNote02Icon,
  'Archetype John Mayer X': MusicNote02Icon,
  'Archetype Nolly X': MusicNote02Icon,
  'Fortin Nameless Suite X': MusicNote02Icon,
  'Microsoft Edge Dev': BrowserIcon,
  'Microsoft Outlook': Mail01Icon,
  'Microsoft Teams': UserMultiple02Icon,
  'Proton Drive': HardDriveIcon,
  'Proton Pass': SecurityPasswordIcon,
  'Zed Preview': CodeSquareIcon,
  Code: VisualStudioCodeIcon,
  Dia: ArcBrowserIcon,
  Discord: DiscordIcon,
  Figma: FigmaIcon,
  Finder: AppleFinderIcon,
  Ghostty: ComputerTerminal01Icon,
  Reminders: AppleReminderIcon,
  Safari: SafariIcon,
  Settings: Settings01Icon,
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
  const { workspaces, focusedApp, apps, isLaptopScreen, onSpaceClick, onAppClick } = useSpaces();

  if (!workspaces) {
    return null;
  }

  return (
    <Stack className={styles.spaces} data-test-id="spaces-container">
      <Surface className={styles.workspaces}>
        {workspaces.map(({ key, name, isFocused }) => (
          <Button
            key={key}
            className={cx(styles.workspace, isFocused && styles.workspaceActive)}
            active={isFocused}
            onClick={onSpaceClick(name)}
          >
            <Icon icon={workspaceIcons[name]} />
          </Button>
        ))}
      </Surface>

      {apps.map(({ key, appName, windowId }) => {
        const isFocused = focusedApp?.windowId === windowId;

        if (isLaptopScreen && !isFocused) {
          return null;
        }

        return (
          <Surface
            as={Button}
            key={key}
            className={cx(styles.app, isFocused && styles.appFocused)}
            onClick={!isFocused ? onAppClick(windowId) : undefined}
          >
            <Icon icon={getAppIcon(appName)} />
            {isFocused && <span>{appName}</span>}
          </Surface>
        );
      })}
    </Stack>
  );
};
