import {
  AppleFinderIcon,
  AppleReminderIcon,
  AppStoreIcon,
  BrowserIcon,
  ComputerTerminal01Icon,
  DashboardCircleIcon,
  DiscordIcon,
  FigmaIcon,
  HardDriveIcon,
  Mail01Icon,
  SecurityPasswordIcon,
  SlackIcon,
  SourceCodeCircleIcon,
  SpotifyIcon,
  UserMultiple02Icon,
  VisualStudioCodeIcon,
  WhatsappIcon,
  ZoomIcon,
} from '@hugeicons/core-free-icons';
import type { QueryClient } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import type { CLIEventPayload } from '@/types';

import type { HyprspaceWindowsPayload } from './CurrentApp.types';

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

export const fetchCurrentHyprspaceWindow = async () => {
  return await invoke<HyprspaceWindowsPayload>('get_hyprspace_focused_window');
};

export const onCLIEvent = ({ name }: CLIEventPayload, queryClient: QueryClient) => {
  if (name !== 'focus-changed') {
    return;
  }

  queryClient.refetchQueries({ queryKey: ['hyprspace_focused_window'] });
};

export const getAppIcon = (name: string) => {
  const appName = name.trim() as keyof typeof appIcons;

  return appIcons[appName] || DashboardCircleIcon;
};
