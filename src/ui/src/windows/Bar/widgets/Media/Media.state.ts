import {
  PlayCircle02Icon,
  SpotifyIcon,
  Vynil02Icon,
  YoutubeIcon,
} from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';

import type { IconSource } from '@/components/Icon';
import type { BarSnapshotOutcome, MediaReading } from '@/ipc';

import type { IconTone } from '../../styles/widgetIcon.style';

/** The media reading while the Bar is available, otherwise `null` (hidden). */
export function selectMedia(outcome: BarSnapshotOutcome): MediaReading | null {
  return outcome.kind === 'available' ? (outcome.view.media ?? null) : null;
}

/** The paused prefix, shown before the label while the media is not playing. */
export const PAUSED_PREFIX = 'Paused: ';

/** What Media's text shows: an optional prefix and the scrolling label. */
export type MediaLabel = { prefix: string | null; label: string };

/** `Title - Artist`, the title alone without an artist, `Paused: ` while not playing. */
export function mediaLabel(
  reading: Pick<MediaReading, 'title' | 'artist' | 'playing'>,
): MediaLabel {
  return {
    prefix: reading.playing ? null : PAUSED_PREFIX,
    label: reading.artist ? `${reading.title} - ${reading.artist}` : reading.title,
  };
}

/** A player's icon and its tone (theme colors). */
export type PlayerIcon = { icon: IconSource; tone: IconTone };

/** The icon of an unknown player. */
export const DEFAULT_PLAYER_ICON: PlayerIcon = { icon: PlayCircle02Icon, tone: 'text' };

/**
 * Known players by bundle identifier, as today. Hugeicons free has no TIDAL
 * logo, so TIDAL uses the simple-icons brand.
 */
export const PLAYER_ICONS: Readonly<Record<string, PlayerIcon>> = {
  'com.spotify.client': { icon: SpotifyIcon, tone: 'success' },
  'com.microsoft.edgemac.Dev': { icon: YoutubeIcon, tone: 'danger' },
  'com.tidal.desktop': { icon: SiTidal, tone: 'text' },
  'org.jeffvli.feishin': { icon: Vynil02Icon, tone: 'highlight' },
};

/** The player icon for a bundle identifier; the play circle for an unknown one. */
export function playerIcon(bundleIdentifier: string): PlayerIcon {
  return Object.hasOwn(PLAYER_ICONS, bundleIdentifier)
    ? PLAYER_ICONS[bundleIdentifier]!
    : DEFAULT_PLAYER_ICON;
}
