import {
  SpotifyIcon,
  YoutubeIcon,
  PlayCircle02Icon,
  Vynil02Icon,
} from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';

import { colors } from '../../../../sdk/tokens';
import type { IconProps } from '../../../components/Icon';

import type { MediaApp } from './Media.types';

/**
 * Known media applications with their bundle identifiers and display names.
 * The fixed click map lands with the media part; today it only selects icons.
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
