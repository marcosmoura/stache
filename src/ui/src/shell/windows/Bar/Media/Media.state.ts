import { useEffect, useState } from 'react';

import { useBarData } from '../Bar.data';

import { getPlayerIconProps } from './Media.constants';
import type { MediaPayload, MediaState, TransformedMediaPayload } from './Media.types';

/**
 * Transforms a raw media payload into a display-ready format.
 * Constructs the label from title and artist, and adds a prefix for paused state.
 */
function parseMediaPayload(media: MediaPayload | null): TransformedMediaPayload | null {
  if (!media) {
    return null;
  }

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
}

/**
 * Hook that manages media playback state and artwork loading.
 *
 * - Reads the current media from the Bar data seam (`null` hides Media)
 * - Preloads artwork images
 * - Returns the icon for the media source
 *
 * The click is wired in `Media.tsx` (`bar_action('media')`); the host maps the published
 * bundle identifier to a fixed target. A payload without artwork keeps the image already
 * loaded, like legacy.
 */
export function useMedia(): MediaState {
  const [loadedArtwork, setLoadedArtwork] = useState<string | null>(null);

  const { media: rawMedia } = useBarData();

  // Transform the media payload
  const media = parseMediaPayload(rawMedia);

  // Preload artwork image to prevent flickering
  useEffect(() => {
    const artwork = media?.artwork;

    if (!artwork) {
      return;
    }

    const image = `data:image/png;base64,${artwork}`;
    const controller = new AbortController();
    const imageLoader = new Image();

    imageLoader.onload = () => {
      if (!controller.signal.aborted) {
        setLoadedArtwork(image);
      }
    };

    imageLoader.onerror = () => {
      if (!controller.signal.aborted) {
        setLoadedArtwork(null);
      }
    };

    imageLoader.src = image;

    return () => controller.abort();
  }, [media?.artwork]);

  const mediaIconProps = getPlayerIconProps(media?.bundleIdentifier ?? '');

  return { media, loadedArtwork, mediaIconProps };
}
