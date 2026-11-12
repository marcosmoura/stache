import {
  AppleFinderIcon,
  DashboardCircleIcon,
  PlayCircleIcon,
  SpotifyIcon,
  WhatsappIcon,
} from '@hugeicons/core-free-icons';
import { SiGhostty, SiTidal } from '@icons-pack/react-simple-icons';
import { describe, expect, it } from 'vitest';

import { Icon } from '@/components/Icon';

import { USER_WORKSPACES } from '../../../../../tests/fixtures';
import { render } from '../../../../../tests/render';

import {
  APP_ICONS,
  appIcon,
  DEFAULT_APP_ICON,
  DEFAULT_WORKSPACE_ICON,
  GENERIC,
  WORKSPACE_ICONS,
  workspaceIcon,
} from './icons';

/** Every application the spec requires an entry for. */
const REQUIRED_APPS = [
  // The user's Tiling configuration (2026-11-11).
  'Ghostty',
  'DeviceHub',
  'Herdr',
  'Code',
  'Zed',
  'Zed Preview',
  'Aside',
  'Safari',
  'Spotify',
  'AIOStreams',
  'Nuvio',
  'WhatsApp',
  'Discord',
  'Slack',
  'Figma',
  'FigCraft',
  'Transmission',
  'Audio MIDI Setup',
  'Ableton Live',
  'Archetype Gojira X',
  'Archetype John Mayer X',
  'Archetype Nolly X',
  'Fortin Nameless Suite X',
  'Soldano SLO100 X',
  'Finder',
  'Bloom',
  'Reminders',
  'Proton Pass',
  'Proton VPN',
  // The other entries `main` has.
  'Activity Monitor',
  'App Store',
  'Microsoft Edge Dev',
  'Proton Drive',
  'Dia',
  'Feishin',
  'Settings',
  'TIDAL',
  'Zoom',
  // Apple apps.
  'Mail',
  'Calendar',
  'Notes',
  'Messages',
  'FaceTime',
  'Music',
  'Podcasts',
  'TV',
  'Photos',
  'Preview',
  'Maps',
  'Books',
  'Calculator',
  'System Settings',
  'Terminal',
  'Xcode',
  'Simulator',
  'App Store',
  'Activity Monitor',
  // Browsers.
  'Google Chrome',
  'Firefox',
  'Arc',
  'Brave Browser',
  'Microsoft Edge',
  'Opera',
  'Vivaldi',
  'Orion',
  // Terminals and editors.
  'iTerm2',
  'Warp',
  'kitty',
  'Alacritty',
  'WezTerm',
  'Cursor',
  'Sublime Text',
  'IntelliJ IDEA',
  'WebStorm',
  'PyCharm',
  'Android Studio',
  'Neovide',
  // Work and chat.
  'Notion',
  'Obsidian',
  'Linear',
  'Telegram',
  'Signal',
  'Microsoft Teams',
  'Microsoft Outlook',
  'Microsoft Word',
  'Microsoft Excel',
  'Microsoft PowerPoint',
  'zoom.us',
  'Google Meet',
  'ChatGPT',
  'Claude',
  // Tools.
  '1Password',
  'Bitwarden',
  'Docker Desktop',
  'Postman',
  'GitHub Desktop',
  'TablePlus',
  'Raycast',
  // Media and design.
  'VLC',
  'IINA',
  'OBS',
  'Steam',
  'TIDAL',
  'Sketch',
  'Adobe Photoshop',
  'Adobe Illustrator',
  'Blender',
];

/* The user's workspaces plus the older `music` and `guitar`. */
const WORKSPACES = [...USER_WORKSPACES, 'music', 'guitar'];

const svgOf = async (icon: Parameters<typeof Icon>[0]['icon']) => {
  const screen = await render(<Icon icon={icon} />);
  const svg = screen.container.querySelector('svg');

  expect(svg).not.toBeNull();
  expect(svg!.querySelector('path, circle, rect, line, polyline, polygon, ellipse')).not.toBeNull();
  screen.unmount();
};

describe('Spaces icons', () => {
  it('has an entry for every required application', () => {
    const missing = REQUIRED_APPS.filter((name) => !Object.hasOwn(APP_ICONS, name));

    expect(missing).toEqual([]);
  });

  it('has an icon for every user workspace, music and guitar, and a default', () => {
    expect(Object.keys(WORKSPACE_ICONS).sort()).toEqual([...WORKSPACES].sort());
    expect(workspaceIcon('workspace-1')).toBe(DEFAULT_WORKSPACE_ICON);
    expect(workspaceIcon('toString')).toBe(DEFAULT_WORKSPACE_ICON);
    expect(DEFAULT_WORKSPACE_ICON).toBe(DashboardCircleIcon);
    expect(workspaceIcon('media')).toBe(PlayCircleIcon);
  });

  it('resolves names trimmed and without a leading U+200E', () => {
    expect(appIcon('\u200EWhatsApp')).toBe(WhatsappIcon);
    expect(appIcon('  \u200EWhatsApp ')).toBe(WhatsappIcon);
    expect(appIcon('WhatsApp')).toBe(WhatsappIcon);
    expect(appIcon('  Finder  ')).toBe(AppleFinderIcon);
    expect(appIcon('Spotify')).toBe(SpotifyIcon);
    expect(appIcon('Ghostty')).toBe(SiGhostty);
    expect(appIcon('TIDAL')).toBe(SiTidal);
  });

  it('uses the default for unknown names', () => {
    expect(appIcon('Nope')).toBe(DEFAULT_APP_ICON);
    expect(appIcon('')).toBe(DEFAULT_APP_ICON);
    expect(appIcon('constructor')).toBe(DEFAULT_APP_ICON);
    expect(appIcon('spotify')).toBe(DEFAULT_APP_ICON);
    expect(DEFAULT_APP_ICON).toBe(DashboardCircleIcon);
  });

  it.each(Object.keys(WORKSPACE_ICONS))('renders the %s workspace icon', async (name) => {
    await svgOf(workspaceIcon(name));
  });

  it.each(Object.keys(APP_ICONS))('renders the icon of %j', async (name) => {
    const icon = APP_ICONS[name]!;

    expect(icon).toBeTruthy();
    await svgOf(icon);
  });

  it.each(Object.keys(GENERIC))('renders the %s category icon', async (category) => {
    await svgOf(GENERIC[category as keyof typeof GENERIC]);
  });
});
