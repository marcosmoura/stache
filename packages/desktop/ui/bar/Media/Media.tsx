import { useCallback, useEffect, useState } from 'react';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { Surface } from '@/components/Surface';
import { colors } from '@/design-system';
import { useTauriEventQuery } from '@/hooks';

import {
  fetchCurrentMedia,
  getPlayerIcon,
  loadMediaArtwork,
  openMediaApp,
  parseMediaPayload,
} from './Media.service';
import * as styles from './Media.styles';
import type { MediaPayload, TransformedMediaPayload } from './Media.types';

export const Media = () => {
  const { data: media } = useTauriEventQuery<MediaPayload, TransformedMediaPayload>({
    eventName: 'tauri_media_changed',
    transformFn: (payload) => parseMediaPayload(payload),
    initialFetch: fetchCurrentMedia,
    queryOptions: {
      refetchOnMount: true,
      staleTime: 5 * 60 * 1000, // 5 minutes
    },
  });

  const [loadedArtwork, setLoadedArtwork] = useState<string | null>(null);

  const onMediaClick = useCallback(() => openMediaApp(media), [media]);

  useEffect(() => {
    if (!media?.artwork) {
      return;
    }

    return loadMediaArtwork(media.artwork, (image) => setLoadedArtwork(image));
  }, [media?.artwork]);

  if (!media?.label) {
    return null;
  }

  const { svg, color } = getPlayerIcon(media?.bundleIdentifier || '');

  return (
    <Surface
      className={styles.media}
      as={Button}
      onClick={onMediaClick}
      data-test-id="media-container"
    >
      {loadedArtwork && <img className={styles.artwork} src={loadedArtwork} alt={media.label} />}
      <Icon icon={svg} fill={color} color={colors.crust} size={22} />
      {media.prefix && <span>{media.prefix}</span>}
      <ScrollingLabel className={styles.label}>{media.label}</ScrollingLabel>
    </Surface>
  );
};
