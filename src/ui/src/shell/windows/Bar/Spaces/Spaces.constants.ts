import {
  ComputerTerminal01Icon,
  CodeSimpleIcon,
  AiBrowserIcon,
  MusicNote03Icon,
  FigmaIcon,
  MessageMultiple01Icon,
  DashboardCircleIcon,
  Folder01Icon,
  CheckListIcon,
  Analytics01Icon,
  AppStoreIcon,
  MixerIcon,
  MusicNote02Icon,
  BrowserIcon,
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
  Vynil02Icon,
  CommandLineIcon,
  SmartPhone01Icon,
  PlayListIcon,
  Tv01Icon,
  PaintBoardIcon,
} from '@hugeicons/core-free-icons';
import { SiProtonvpn, SiTidal, SiTransmission } from '@icons-pack/react-simple-icons';

import { motionRaw } from '../../../../sdk/tokens';
import type { AnyIcon } from '../../../components/Icon';

/** Verbatim from `_legacy/app/ui/renderer/bar/Spaces/Spaces.constants.ts`. */
export const workspaceIcons: Record<string, AnyIcon> = {
  terminal: ComputerTerminal01Icon,
  coding: CodeSimpleIcon,
  browser: AiBrowserIcon,
  music: MusicNote03Icon,
  design: FigmaIcon,
  communication: MessageMultiple01Icon,
  guitar: Vynil02Icon,
  misc: DashboardCircleIcon,
  files: Folder01Icon,
  tasks: CheckListIcon,
};

/**
 * Every legacy entry verbatim, plus the entries approved on 2026-11-10, keyed by
 * the exact `CFBundleName` (Zed, Aside, Herdr, DeviceHub, AIOStreams, Nuvio, FigCraft).
 */
export const appIcons = {
  'Ableton Live': MusicNote02Icon,
  'Activity Monitor': Analytics01Icon,
  'App Store': AppStoreIcon,
  'Archetype Gojira X': MusicNote02Icon,
  'Archetype John Mayer X': MusicNote02Icon,
  'Archetype Nolly X': MusicNote02Icon,
  'Audio MIDI Setup': MixerIcon,
  'Fortin Nameless Suite X': MusicNote02Icon,
  'Microsoft Edge Dev': BrowserIcon,
  'Proton Drive': HardDriveIcon,
  'Proton Pass': SecurityPasswordIcon,
  'Proton VPN': SiProtonvpn,
  'Soldano SLO100 X': MusicNote02Icon,
  // WhatsApp's bundle name starts with an invisible U+200E LEFT-TO-RIGHT MARK (legacy key).
  '\u200EWhatsApp': WhatsappIcon,
  'Zed Preview': CodeSquareIcon,
  Bloom: Folder01Icon,
  Code: VisualStudioCodeIcon,
  Dia: ArcBrowserIcon,
  Discord: DiscordIcon,
  Feishin: MusicNote02Icon,
  Figma: FigmaIcon,
  Finder: AppleFinderIcon,
  Ghostty: ComputerTerminal01Icon,
  Reminders: AppleReminderIcon,
  Safari: SafariIcon,
  Settings: Settings01Icon,
  Slack: SlackIcon,
  Spotify: SpotifyIcon,
  TIDAL: SiTidal,
  Transmission: SiTransmission,
  WhatsApp: WhatsappIcon,
  Zoom: ZoomIcon,
  // Approved additions (2026-11-10).
  Zed: CodeSquareIcon,
  Aside: AiBrowserIcon,
  Herdr: CommandLineIcon,
  DeviceHub: SmartPhone01Icon,
  AIOStreams: PlayListIcon,
  Nuvio: Tv01Icon,
  FigCraft: PaintBoardIcon,
} as const;

export function getAppIcon(name: string): AnyIcon {
  const appName = name.trim() as keyof typeof appIcons;

  return appIcons[appName] || DashboardCircleIcon;
}

export const ease = motionRaw.easing.split(',').map(Number) as [number, number, number, number];

export const springTransition = {
  type: 'spring',
  bounce: 0,
  duration: motionRaw.durationSlower,
} as const;
