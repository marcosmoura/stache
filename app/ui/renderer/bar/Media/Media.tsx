import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { Surface } from '@/components/Surface';

import { useMedia } from './Media.state';
import * as styles from './Media.styles';

export const Media = () => {
  const { media, loadedArtwork, onMediaClick, mediaIconProps } = useMedia();

  if (!media?.label) {
    return null;
  }

  return (
    <Surface
      className={styles.media}
      as={Button}
      onClick={onMediaClick}
      data-test-id="media-container"
    >
      {loadedArtwork && <img className={styles.artwork} src={loadedArtwork} alt={media.label} />}
      <Icon {...mediaIconProps} />
      {media.prefix && <span>{media.prefix}</span>}
      <ScrollingLabel className={styles.label}>{media.label}</ScrollingLabel>
    </Surface>
  );
};
