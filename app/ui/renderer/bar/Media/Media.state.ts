import { useCallback, useEffect, useState } from 'react';

import { type IconDefinition, faTidal } from '@fortawesome/free-brands-svg-icons';
import { SpotifyIcon, YoutubeIcon, PlayCircle02Icon } from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';
import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';
import { useTauriEventQuery } from '@/hooks';
import { MediaEvents } from '@/types';

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
  tidal: {
    bundleIdentifier: 'com.tidal.desktop',
    name: 'Tidal',
  },
};

const mediaAppsByBundleId = Object.values(mediaApps).reduce<Record<string, MediaApp>>(
  (acc, app) => {
    acc[app.bundleIdentifier] = app;
    return acc;
  },
  {},
);

type PlayerIconInfo = {
  icon: IconSvgElement | IconDefinition;
  color: string;
  pack: 'hugeicons' | 'fontawesome';
};

const getPlayerIcon = (bundleIdentifier: string): PlayerIconInfo => {
  switch (bundleIdentifier) {
    case mediaApps.spotify.bundleIdentifier:
      return {
        icon: SpotifyIcon,
        color: colors.green,
        pack: 'hugeicons',
      };
    case mediaApps.edge.bundleIdentifier:
      return {
        icon: YoutubeIcon,
        color: colors.red,
        pack: 'hugeicons',
      };
    case mediaApps.tidal.bundleIdentifier:
      return {
        icon: faTidal,
        color: colors.text,
        pack: 'fontawesome',
      };
    default:
      return {
        icon: PlayCircle02Icon,
        color: colors.text,
        pack: 'hugeicons',
      };
  }
};

const fetchCurrentMedia = async (): Promise<MediaPayload | null> => {
  try {
    const payload = await invoke<MediaPayload | null>('get_current_media_info');
    return payload ?? null;
  } catch (error) {
    console.error('Failed to fetch media information', error);
    return null;
  }
};

const parseMediaPayload = (media: MediaPayload): TransformedMediaPayload => {
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

export const useMedia = () => {
  const [loadedArtwork, setLoadedArtwork] = useState<string | null>(null);

  const { data: media } = useTauriEventQuery<MediaPayload, TransformedMediaPayload>({
    eventName: MediaEvents.PLAYBACK_CHANGED,
    transformFn: (payload) => parseMediaPayload(payload),
    initialFetch: fetchCurrentMedia,
    queryOptions: {
      refetchOnMount: true,
      staleTime: 5 * 60 * 1000, // 5 minutes
    },
  });

  const onMediaClick = useCallback(async () => {
    if (!media?.bundleIdentifier) {
      return;
    }

    const targetApp = mediaAppsByBundleId[media.bundleIdentifier];

    if (!targetApp) {
      return;
    }

    await invoke('open_app', { name: targetApp.name });
  }, [media]);

  useEffect(() => {
    const artwork = media?.artwork;

    if (!artwork) {
      return;
    }

    const image = `data:image/png;base64,${artwork}`;
    const imageLoader = new Image();

    imageLoader.src = image;
    imageLoader.onload = () => setLoadedArtwork(image);
    imageLoader.onerror = () => setLoadedArtwork(null);

    return () => {
      imageLoader.src = '';
      imageLoader.onload = null;
      imageLoader.onerror = null;
    };
  }, [media?.artwork]);

  const mediaIcon = getPlayerIcon(media?.bundleIdentifier || '');

  return { media, loadedArtwork, onMediaClick, mediaIcon };
};
