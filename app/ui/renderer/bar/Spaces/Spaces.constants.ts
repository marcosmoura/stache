import {
  ComputerTerminal01Icon,
  CodeSimpleIcon,
  AiBrowserIcon,
  MusicNote03Icon,
  FigmaIcon,
  MessageMultiple01Icon,
  DashboardCircleIcon,
  Folder01Icon,
  MailAtSign01Icon,
  CheckListIcon,
  Analytics01Icon,
  AppStoreIcon,
  MusicNote02Icon,
  BrowserIcon,
  Mail01Icon,
  UserMultiple02Icon,
  HardDriveIcon,
  SecurityPasswordIcon,
  CodeSquareIcon,
  VisualStudioCodeIcon,
  ArcBrowserIcon,
  DiscordIcon,
  AppleFinderIcon,
  AppleReminderIcon,
  SafariIcon,
  Settings01Icon,
  SlackIcon,
  SpotifyIcon,
  WhatsappIcon,
  ZoomIcon,
} from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';

import { motionRaw } from '@/design-system';

export const workspaceIcons: Record<string, IconSvgElement> = {
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

export const getAppIcon = (name: string) => {
  const appName = name.trim() as keyof typeof appIcons;

  return appIcons[appName] || DashboardCircleIcon;
};

export const ease = motionRaw.easing.split(',').map(Number) as [number, number, number, number];

export const springTransition = {
  type: 'spring',
  bounce: 0,
  duration: motionRaw.durationSlower,
} as const;
