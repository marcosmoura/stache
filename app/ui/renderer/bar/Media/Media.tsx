import type { IconDefinition } from '@fortawesome/fontawesome-svg-core';
import type { IconSvgElement } from '@hugeicons/react';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { Surface } from '@/components/Surface';

import { useMedia } from './Media.state';
import * as styles from './Media.styles';

export const Media = () => {
  const { media, loadedArtwork, onMediaClick, mediaIcon } = useMedia();

  if (!media?.label) {
    return null;
  }

  const { icon, color, pack } = mediaIcon;

  return (
    <Surface
      className={styles.media}
      as={Button}
      onClick={onMediaClick}
      data-test-id="media-container"
    >
      {loadedArtwork && <img className={styles.artwork} src={loadedArtwork} alt={media.label} />}
      {pack === 'fontawesome' && (
        <Icon pack="fontawesome" icon={icon as IconDefinition} fill={color} />
      )}
      {pack === 'hugeicons' && (
        <Icon pack="hugeicons" icon={icon as IconSvgElement} color={color} size={22} />
      )}
      {media.prefix && <span>{media.prefix}</span>}
      <ScrollingLabel className={styles.label}>{media.label}</ScrollingLabel>
    </Surface>
  );
};
