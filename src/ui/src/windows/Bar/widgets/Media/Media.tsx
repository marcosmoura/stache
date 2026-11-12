import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { useRetainedArtwork } from './Media.artwork';
import { artworkUrl, mediaArtworkOptions } from './Media.query';
import { mediaLabel, playerIcon, selectMedia } from './Media.state';
import { styles } from './Media.style';

/**
 * Now playing: the retained artwork, the player icon, `Paused: ` while not
 * playing and `Title - Artist` in a scrolling label. Hidden while there is no
 * media. Click: `bar_action("media")`.
 */
export function Media(_props: WidgetProps<'media'>) {
  const { data: reading = null } = useQuery({ ...barSnapshotOptions(), select: selectMedia });
  const { data: base64 } = useQuery(mediaArtworkOptions(reading?.artworkId ?? null));
  // Held above the conditional render, so the last image survives a hidden spell.
  const { artwork, fail } = useRetainedArtwork(base64 ? artworkUrl(base64) : null);
  const run = useBarAction();

  if (!reading) {
    return null;
  }

  const { prefix, label } = mediaLabel(reading);
  const player = playerIcon(reading.bundleIdentifier);

  return (
    <BarItem onClick={() => run('media')} style={artwork ? styles.withArtwork : null}>
      {artwork && (
        <img src={artwork} alt={label} onError={fail} {...stylex.props(styles.artwork)} />
      )}
      <Icon
        icon={player.icon}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon[player.tone], styles.icon]}
      />
      {prefix && <span>{prefix}</span>}
      <ScrollingLabel style={styles.label}>{label}</ScrollingLabel>
    </BarItem>
  );
}

export default Media;
