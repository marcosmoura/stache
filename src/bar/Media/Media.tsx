import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { cx } from '@linaria/core';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { colors } from '@/design-system';
import { useTauriEventQuery } from '@/hooks';

import {
  fetchCurrentMedia,
  getPlayerIcon,
  loadMediaArtwork,
  openMediaApp,
  transformMediaPayload,
} from './Media.service';
import * as styles from './Media.styles';
import type { MediaPayload, TransformedMediaPayload } from './Media.types';

export const Media = () => {
  const { data: media } = useTauriEventQuery<MediaPayload, TransformedMediaPayload>({
    eventName: 'tauri_media_changed',
    transformFn: (payload) => transformMediaPayload(payload),
    initialFetch: fetchCurrentMedia,
    queryOptions: {
      refetchOnMount: true,
      staleTime: 5 * 60 * 1000, // 5 minutes
    },
  });

  const [loadedArtwork, setLoadedArtwork] = useState<string | null>(null);
  const labelWrapperRef = useRef<HTMLDivElement>(null);
  const labelRef = useRef<HTMLSpanElement>(null);
  const [scrollDistance, setScrollDistance] = useState(0);

  const onMediaClick = useCallback(() => openMediaApp(media), [media]);

  useEffect(() => {
    if (!media?.artwork) {
      return;
    }

    return loadMediaArtwork(media.artwork, (image) => setLoadedArtwork(image));
  }, [media?.artwork]);

  useEffect(() => {
    const wrapper = labelWrapperRef.current;
    const label = labelRef.current;

    if (!wrapper || !label) {
      return;
    }

    const calculateScrollDistance = () => {
      const wrapperWidth = wrapper.offsetWidth;
      const labelWidth = label.scrollWidth;
      const overflow = labelWidth - wrapperWidth;

      setScrollDistance(overflow > 0 ? -overflow : 0);
    };

    calculateScrollDistance();

    const resizeObserver = new ResizeObserver(calculateScrollDistance);
    resizeObserver.observe(wrapper);
    resizeObserver.observe(label);

    return () => resizeObserver.disconnect();
  }, [media?.label]);

  const isScrolling = scrollDistance < 0;
  const scrollStyles = useMemo(() => {
    // Calculate duration: base 2s + ~30px per second for readable scrolling
    const scrollDuration = Math.max(3, 2 + Math.abs(scrollDistance) / 30);

    return {
      '--scroll-distance': `${scrollDistance}px`,
      '--scroll-duration': `${scrollDuration}s`,
    };
  }, [scrollDistance]);

  if (!media?.label) {
    return null;
  }

  const { svg, color } = getPlayerIcon(media?.bundleIdentifier || '');

  return (
    <Surface className={styles.media} as={Button} onClick={onMediaClick}>
      {loadedArtwork && <img className={styles.artwork} src={loadedArtwork} alt={media.label} />}
      <Icon icon={svg} fill={color} color={colors.crust} size={22} />
      {media.prefix && <span className={styles.label}>{media.prefix}</span>}
      <div ref={labelWrapperRef} className={styles.labelWrapper}>
        <span
          ref={labelRef}
          className={cx(styles.label, isScrolling && styles.scrollingLabel)}
          style={scrollStyles}
        >
          {media.label}
        </span>
      </div>
    </Surface>
  );
};
