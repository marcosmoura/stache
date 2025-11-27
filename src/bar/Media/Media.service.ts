import { SpotifyIcon, YoutubeIcon, PlayCircle02Icon } from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';
import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';

import type { MediaApp, MediaPayload, TransformedMediaPayload } from './Media.types';

const mediaApps = {
  spotify: {
    bundleIdentifier: 'com.spotify.client',
    name: 'Spotify',
  },
  edge: {
    bundleIdentifier: 'com.microsoft.edgemac.Dev',
    name: 'Microsoft Edge Dev',
  },
};

const mediaAppsByBundleId = Object.values(mediaApps).reduce<Record<string, MediaApp>>(
  (acc, app) => {
    acc[app.bundleIdentifier] = app;
    return acc;
  },
  {},
);

export const fetchCurrentMedia = async (): Promise<MediaPayload | null> => {
  try {
    const payload = await invoke<MediaPayload | null>('get_current_media_info');
    return payload ?? null;
  } catch (error) {
    console.error('Failed to fetch media information', error);
    return null;
  }
};

export const parseMediaPayload = (media: MediaPayload): TransformedMediaPayload => {
  const { artist, title, artwork, playing, bundleIdentifier } = media;

  const separator = ' - ';
  const prefix = playing ? '' : 'Paused: ';
  const label = artist ? title + separator + artist : title;

  return {
    artwork,
    bundleIdentifier,
    prefix,
    label,
  };
};

export const openMediaApp = async (media?: TransformedMediaPayload) => {
  if (!media?.bundleIdentifier) {
    return;
  }

  const targetApp = mediaAppsByBundleId[media.bundleIdentifier];

  if (!targetApp) {
    return;
  }

  await invoke('open_app', { name: targetApp.name });
};

export const loadMediaArtwork = (artwork?: string, onLoad?: (image: string | null) => void) => {
  if (!artwork) {
    return;
  }

  const image = `data:image/png;base64,${artwork}`;
  const imageLoader = new Image();

  imageLoader.src = image;
  imageLoader.onload = () => onLoad?.(image);
  imageLoader.onerror = () => onLoad?.(null);

  return () => {
    imageLoader.src = '';
    imageLoader.onload = null;
    imageLoader.onerror = null;
  };
};

export const getPlayerIcon = (bundleIdentifier: string): { svg: IconSvgElement; color: string } => {
  switch (bundleIdentifier) {
    case mediaApps.spotify.bundleIdentifier:
      return {
        svg: SpotifyIcon,
        color: colors.green,
      };
    case mediaApps.edge.bundleIdentifier:
      return {
        svg: YoutubeIcon,
        color: colors.red,
      };
    default:
      return {
        svg: PlayCircle02Icon,
        color: colors.text,
      };
  }
};
