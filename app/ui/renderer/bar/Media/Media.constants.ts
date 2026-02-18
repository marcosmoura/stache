import {
  SpotifyIcon,
  YoutubeIcon,
  PlayCircle02Icon,
  Vynil02Icon,
} from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';

import type { IconProps } from '@/components/Icon';
import { colors } from '@/design-system';

import type { MediaApp } from './Media.types';

/**
 * Known media applications with their bundle identifiers and display names.
 * Add new media apps here to enable icon mapping and click-to-open functionality.
 */
export const MEDIA_APPS = {
  spotify: {
    bundleIdentifier: 'com.spotify.client',
    name: 'Spotify',
  },
  edge: {
    bundleIdentifier: 'com.microsoft.edgemac.Dev',
    name: 'Microsoft Edge Dev',
  },
  tidal: {
    bundleIdentifier: 'com.tidal.desktop',
    name: 'Tidal',
  },
  feishin: {
    bundleIdentifier: 'org.jeffvli.feishin',
    name: 'Feishin',
  },
} as const satisfies Record<string, MediaApp>;

/**
 * Lookup table for media apps by their bundle identifier.
 */
export const MEDIA_APPS_BY_BUNDLE_ID = Object.values(MEDIA_APPS).reduce<Record<string, MediaApp>>(
  (acc, app) => {
    acc[app.bundleIdentifier] = app;
    return acc;
  },
  {},
);

/**
 * Default icon props for unknown media players.
 */
const DEFAULT_PLAYER_ICON: IconProps = {
  icon: PlayCircle02Icon,
  color: colors.text,
  size: 22,
};

/**
 * Icon props for known media players, keyed by bundle identifier.
 */
const PLAYER_ICONS: Record<string, IconProps> = {
  [MEDIA_APPS.spotify.bundleIdentifier]: {
    icon: SpotifyIcon,
    color: colors.green,
    size: 22,
  },
  [MEDIA_APPS.edge.bundleIdentifier]: {
    icon: YoutubeIcon,
    color: colors.red,
    size: 22,
  },
  [MEDIA_APPS.tidal.bundleIdentifier]: {
    icon: SiTidal,
    color: colors.text,
    size: 18,
  },
  [MEDIA_APPS.feishin.bundleIdentifier]: {
    icon: Vynil02Icon,
    color: colors.sky,
    size: 22,
  },
};

/**
 * Gets the icon props for a media player by its bundle identifier.
 * Returns a default play icon for unknown players.
 */
export const getPlayerIconProps = (bundleIdentifier: string): IconProps => {
  return PLAYER_ICONS[bundleIdentifier] ?? DEFAULT_PLAYER_ICON;
};
