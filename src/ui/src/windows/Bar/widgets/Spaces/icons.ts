import {
  AdobeIllustratorIcon,
  AdobePhotoshopIcon,
  AppleFinderIcon,
  AppleMusicIcon,
  AppleReminderIcon,
  AppStoreIcon,
  ArcBrowserIcon,
  BlenderIcon,
  Book01Icon,
  BrowserIcon,
  Calendar03Icon,
  CalculatorIcon,
  ChatGptIcon,
  CheckListIcon,
  ChromeIcon,
  ClaudeIcon,
  CodeSquareIcon,
  ComputerTerminal01Icon,
  DashboardCircleIcon,
  DiscordIcon,
  FigmaIcon,
  Folder01Icon,
  GithubIcon,
  Image01Icon,
  Mail01Icon,
  MapsIcon,
  MessageMultiple01Icon,
  MicrosoftTeamsIcon,
  MusicNote02Icon,
  MusicNote03Icon,
  Note01Icon,
  NotionIcon,
  PaintBoardIcon,
  PlayCircleIcon,
  SafariIcon,
  SecurityPasswordIcon,
  Settings01Icon,
  SketchIcon,
  SlackIcon,
  SmartPhone01Icon,
  SourceCodeIcon,
  SpotifyIcon,
  TelegramIcon,
  Video01Icon,
  VisualStudioCodeIcon,
  Vynil02Icon,
  WhatsappIcon,
  ZoomIcon,
} from '@hugeicons/core-free-icons';
import {
  Si1password,
  SiAlacritty,
  SiAndroidstudio,
  SiApplepodcasts,
  SiAppletv,
  SiBitwarden,
  SiBrave,
  SiCursor,
  SiDocker,
  SiFirefox,
  SiGhostty,
  SiGooglemeet,
  SiIntellijidea,
  SiIterm2,
  SiLinear,
  SiObsidian,
  SiObsstudio,
  SiOpera,
  SiPostman,
  SiProtondrive,
  SiProtonvpn,
  SiPycharm,
  SiRaycast,
  SiSignal,
  SiSteam,
  SiSublimetext,
  SiTidal,
  SiTransmission,
  SiVivaldi,
  SiVlcmediaplayer,
  SiWarp,
  SiWebstorm,
  SiWezterm,
  SiXcode,
  SiZedindustries,
} from '@icons-pack/react-simple-icons';

import type { IconSource } from '@/components/Icon';

/*
 * The Spaces icon tables, loaded with the widget chunk. An app's icon is, in
 * order: a Hugeicons free brand icon, a simple-icons brand, or the Hugeicons
 * icon of its category (`GENERIC`). Keys are exact application names
 * (`CFBundleName`, or the published name for apps not installed where the
 * table was written).
 */

/**
 * The workspace icons by name (the user's workspaces and the older `music` and
 * `guitar`); any other name uses {@link DEFAULT_WORKSPACE_ICON}.
 */
export const WORKSPACE_ICONS: Readonly<Record<string, IconSource>> = {
  terminal: ComputerTerminal01Icon,
  coding: SiZedindustries,
  browser: SafariIcon,
  music: MusicNote03Icon,
  design: FigmaIcon,
  communication: MessageMultiple01Icon,
  guitar: Vynil02Icon,
  media: PlayCircleIcon,
  misc: DashboardCircleIcon,
  files: Folder01Icon,
  tasks: CheckListIcon,
};

/** The icon of a workspace name the table does not know. */
export const DEFAULT_WORKSPACE_ICON: IconSource = DashboardCircleIcon;

/** The icon of an application name the table does not know. */
export const DEFAULT_APP_ICON: IconSource = DashboardCircleIcon;

/** The category icons for apps with no brand icon in either set. */
export const GENERIC = {
  browser: BrowserIcon,
  terminal: ComputerTerminal01Icon,
  codeEditor: CodeSquareIcon,
  chat: MessageMultiple01Icon,
  music: MusicNote02Icon,
  video: Video01Icon,
  design: PaintBoardIcon,
  office: Note01Icon,
  mail: Mail01Icon,
  calendar: Calendar03Icon,
  photos: Image01Icon,
  maps: MapsIcon,
  books: Book01Icon,
  calculator: CalculatorIcon,
  security: SecurityPasswordIcon,
  developerTool: SourceCodeIcon,
  device: SmartPhone01Icon,
  files: Folder01Icon,
  system: Settings01Icon,
} as const satisfies Record<string, IconSource>;

/** App icons by exact application name. */
export const APP_ICONS: Readonly<Record<string, IconSource>> = {
  // The user's Tiling configuration (2026-11-11).
  Ghostty: SiGhostty,
  DeviceHub: GENERIC.device,
  Herdr: GENERIC.terminal,
  Code: VisualStudioCodeIcon,
  Zed: SiZedindustries,
  'Zed Preview': SiZedindustries,
  Aside: GENERIC.browser,
  Safari: SafariIcon,
  Spotify: SpotifyIcon,
  AIOStreams: GENERIC.video,
  Nuvio: GENERIC.video,
  // WhatsApp's bundle name starts with U+200E; lookups drop it.
  WhatsApp: WhatsappIcon,
  Discord: DiscordIcon,
  Slack: SlackIcon,
  Figma: FigmaIcon,
  FigCraft: GENERIC.design,
  Transmission: SiTransmission,
  'Audio MIDI Setup': GENERIC.music,
  'Ableton Live': GENERIC.music,
  Live: GENERIC.music,
  'Archetype Cory Wong X': GENERIC.music,
  'Archetype Gojira X': GENERIC.music,
  'Archetype John Mayer X': GENERIC.music,
  'Archetype Misha Mansoor X': GENERIC.music,
  'Archetype Nolly X': GENERIC.music,
  'Archetype Petrucci X': GENERIC.music,
  'Archetype Rabea X': GENERIC.music,
  'Archetype Tim Henson X': GENERIC.music,
  'Fortin Nameless Suite X': GENERIC.music,
  'Soldano SLO-100 X': GENERIC.music,
  'Soldano SLO100 X': GENERIC.music,
  Finder: AppleFinderIcon,
  Bloom: GENERIC.files,
  Reminders: AppleReminderIcon,
  'Proton Pass': GENERIC.security,
  'Proton VPN': SiProtonvpn,
  ProtonVPN: SiProtonvpn,
  // The other entries `main` has.
  'Activity Monitor': GENERIC.system,
  'App Store': AppStoreIcon,
  'Microsoft Edge Dev': GENERIC.browser,
  'Proton Drive': SiProtondrive,
  Dia: GENERIC.browser,
  Feishin: GENERIC.music,
  Settings: GENERIC.system,
  TIDAL: SiTidal,
  Zoom: ZoomIcon,
  // Apple apps.
  Mail: GENERIC.mail,
  Calendar: GENERIC.calendar,
  Notes: GENERIC.office,
  Messages: GENERIC.chat,
  FaceTime: GENERIC.video,
  Music: AppleMusicIcon,
  Podcasts: SiApplepodcasts,
  TV: SiAppletv,
  Photos: GENERIC.photos,
  Preview: GENERIC.photos,
  Maps: GENERIC.maps,
  Books: GENERIC.books,
  Calculator: GENERIC.calculator,
  'System Settings': GENERIC.system,
  Terminal: GENERIC.terminal,
  Xcode: SiXcode,
  Simulator: GENERIC.device,
  // Browsers.
  'Google Chrome': ChromeIcon,
  Firefox: SiFirefox,
  Arc: ArcBrowserIcon,
  'Brave Browser': SiBrave,
  'Microsoft Edge': GENERIC.browser,
  Opera: SiOpera,
  Vivaldi: SiVivaldi,
  Orion: GENERIC.browser,
  // Terminals and editors.
  iTerm2: SiIterm2,
  Warp: SiWarp,
  kitty: GENERIC.terminal,
  Alacritty: SiAlacritty,
  WezTerm: SiWezterm,
  Cursor: SiCursor,
  'Sublime Text': SiSublimetext,
  'IntelliJ IDEA': SiIntellijidea,
  WebStorm: SiWebstorm,
  PyCharm: SiPycharm,
  'Android Studio': SiAndroidstudio,
  Neovide: GENERIC.codeEditor,
  // Work and chat.
  Notion: NotionIcon,
  Obsidian: SiObsidian,
  Linear: SiLinear,
  Telegram: TelegramIcon,
  Signal: SiSignal,
  'Microsoft Teams': MicrosoftTeamsIcon,
  'Microsoft Outlook': GENERIC.mail,
  'Microsoft Word': GENERIC.office,
  'Microsoft Excel': GENERIC.office,
  'Microsoft PowerPoint': GENERIC.office,
  'zoom.us': ZoomIcon,
  'Google Meet': SiGooglemeet,
  ChatGPT: ChatGptIcon,
  Claude: ClaudeIcon,
  // Tools.
  '1Password': Si1password,
  Bitwarden: SiBitwarden,
  'Docker Desktop': SiDocker,
  Postman: SiPostman,
  'GitHub Desktop': GithubIcon,
  TablePlus: GENERIC.developerTool,
  Raycast: SiRaycast,
  // Media and design.
  VLC: SiVlcmediaplayer,
  IINA: GENERIC.video,
  OBS: SiObsstudio,
  Steam: SiSteam,
  Sketch: SketchIcon,
  'Adobe Photoshop': AdobePhotoshopIcon,
  'Adobe Illustrator': AdobeIllustratorIcon,
  Blender: BlenderIcon,
};

/** The leading U+200E LEFT-TO-RIGHT MARK some bundle names carry (WhatsApp). */
const LEFT_TO_RIGHT_MARK = /^\u200E+/u;

/** The icon of a workspace name. */
export function workspaceIcon(name: string): IconSource {
  return Object.hasOwn(WORKSPACE_ICONS, name) ? WORKSPACE_ICONS[name]! : DEFAULT_WORKSPACE_ICON;
}

/** The icon of an application name: trimmed, then without a leading U+200E. */
export function appIcon(name: string): IconSource {
  const key = name.trim().replace(LEFT_TO_RIGHT_MARK, '').trim();

  return Object.hasOwn(APP_ICONS, key) ? APP_ICONS[key]! : DEFAULT_APP_ICON;
}
