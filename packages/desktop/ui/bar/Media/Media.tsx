import { useCallback, useEffect, useState } from 'react';

import type { IconDefinition } from '@fortawesome/free-brands-svg-icons';
import { FontAwesomeIcon } from '@fortawesome/react-fontawesome';
import type { IconSvgElement } from '@hugeicons/react';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { Surface } from '@/components/Surface';
import { colors } from '@/design-system';
import { useTauriEventQuery } from '@/hooks';
import { MediaEvents } from '@/types';

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
    eventName: MediaEvents.PLAYBACK_CHANGED,
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

  const { svg, color, iconPack } = getPlayerIcon(media?.bundleIdentifier || '');

  return (
    <Surface
      className={styles.media}
      as={Button}
      onClick={onMediaClick}
      data-test-id="media-container"
    >
      {loadedArtwork && <img className={styles.artwork} src={loadedArtwork} alt={media.label} />}
      {iconPack === 'hugeicons' && (
        <Icon icon={svg as IconSvgElement} fill={color} color={colors.crust} size={22} />
      )}
      {iconPack === 'fontawesome' && <FontAwesomeIcon icon={svg as IconDefinition} />}
      {media.prefix && <span>{media.prefix}</span>}
      <ScrollingLabel className={styles.label}>{media.label}</ScrollingLabel>
    </Surface>
  );
};
