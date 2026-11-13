import {
  PlayCircle02Icon,
  SpotifyIcon,
  Vynil02Icon,
  YoutubeIcon,
} from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';
import { describe, expect, it } from 'vitest';

import { barAvailable, barUnauthorized, barUnavailable } from '../../../../../tests/fixtures';

import {
  DEFAULT_PLAYER_ICON,
  mediaLabel,
  PLAYER_ICONS,
  playerIcon,
  selectMedia,
} from './Media.state';

const reading = {
  title: 'Song',
  artist: 'Band',
  album: null,
  bundleIdentifier: 'com.spotify.client',
  mediaType: null,
  playing: true,
  artworkId: null,
};

describe('Media state', () => {
  it('builds the label and the paused prefix', () => {
    expect(mediaLabel(reading)).toEqual({ prefix: null, label: 'Song - Band' });
    expect(mediaLabel({ ...reading, artist: '' })).toEqual({ prefix: null, label: 'Song' });
    expect(mediaLabel({ ...reading, playing: false })).toEqual({
      prefix: 'Paused: ',
      label: 'Song - Band',
    });
  });

  it('maps every known player to its icon and tone', () => {
    expect(playerIcon('com.spotify.client')).toEqual({ icon: SpotifyIcon, tone: 'success' });
    expect(playerIcon('com.microsoft.edgemac.Dev')).toEqual({ icon: YoutubeIcon, tone: 'danger' });
    expect(playerIcon('com.tidal.desktop')).toEqual({ icon: SiTidal, tone: 'text' });
    expect(playerIcon('org.jeffvli.feishin')).toEqual({ icon: Vynil02Icon, tone: 'highlight' });

    for (const [bundle, entry] of Object.entries(PLAYER_ICONS)) {
      expect(entry.icon, bundle).toBeDefined();
      // A Hugeicons icon, except TIDAL's simple-icons brand.
      expect(Array.isArray(entry.icon), bundle).toBe(bundle !== 'com.tidal.desktop');
    }
  });

  it('uses the play circle for an unknown or empty player', () => {
    expect(DEFAULT_PLAYER_ICON).toEqual({ icon: PlayCircle02Icon, tone: 'text' });
    expect(playerIcon('')).toBe(DEFAULT_PLAYER_ICON);
    expect(playerIcon('com.example.player')).toBe(DEFAULT_PLAYER_ICON);
    expect(playerIcon('toString')).toBe(DEFAULT_PLAYER_ICON);
  });

  it('selects the reading only while available', () => {
    expect(selectMedia(barAvailable('1', { media: reading }))).toEqual(reading);
    expect(selectMedia(barAvailable('1', { media: null }))).toBeNull();
    expect(selectMedia(barUnavailable('1'))).toBeNull();
    expect(selectMedia(barUnauthorized)).toBeNull();
  });
});
