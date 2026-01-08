import { useCallback, useEffect, useState } from 'react';

import { invoke } from '@tauri-apps/api/core';

import { useTauri } from '@/hooks/useTauri';
import { MediaEvents } from '@/types';

import { getPlayerIconProps, MEDIA_APPS_BY_BUNDLE_ID } from './Media.constants';
import type { MediaPayload, TransformedMediaPayload } from './Media.types';

/**
 * Fetches the current media playback information from the Tauri backend.
 * Returns null if no media is playing or if the fetch fails.
 */
const fetchCurrentMedia = async (): Promise<MediaPayload | null> => {
  try {
    const payload = await invoke<MediaPayload | null>('get_current_media_info');
    return payload ?? null;
  } catch (error) {
    // Media info may not be available if no media is playing - this is expected
    console.warn('[Media] Failed to fetch media information:', error);
    return null;
  }
};

/**
 * Transforms a raw media payload into a display-ready format.
 * Constructs the label from title and artist, and adds a prefix for paused state.
 */
const parseMediaPayload = (media: MediaPayload | null): TransformedMediaPayload | null => {
  if (!media) return null;

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

/**
 * Hook that manages media playback state and artwork loading.
 *
 * Features:
 * - Fetches current media info on mount
 * - Subscribes to real-time playback changes
 * - Preloads artwork images
 * - Provides click handler to open the source app
 * - Returns appropriate icon based on the media source
 */
export const useMedia = () => {
  const [loadedArtwork, setLoadedArtwork] = useState<string | null>(null);

  const { data: rawMedia } = useTauri<MediaPayload | null>({
    queryKey: ['media'],
    queryFn: fetchCurrentMedia,
    eventName: MediaEvents.PLAYBACK_CHANGED,
    staleTime: 5 * 60 * 1000, // 5 minutes
  });

  // Transform the media payload
  const media = parseMediaPayload(rawMedia ?? null);

  const onMediaClick = useCallback(async () => {
    if (!media?.bundleIdentifier) {
      return;
    }

    const targetApp = MEDIA_APPS_BY_BUNDLE_ID[media.bundleIdentifier];

    if (!targetApp) {
      return;
    }

    await invoke('open_app', { name: targetApp.name });
  }, [media]);

  // Preload artwork image to prevent flickering
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

  const mediaIconProps = getPlayerIconProps(media?.bundleIdentifier ?? '');

  return { media, loadedArtwork, onMediaClick, mediaIconProps };
};
