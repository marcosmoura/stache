import * as stylex from '@stylexjs/stylex';

import { bar, media, radius } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  /** With the item's 1px border the artwork sits 2px from the leading edge. */
  withArtwork: {
    paddingInlineStart: bar.mediaArtworkInset,
  },
  artwork: {
    overflow: 'hidden',
    flexShrink: 0,
    width: bar.mediaArtworkSize,
    height: bar.mediaArtworkSize,
    borderRadius: radius.lg,
    objectFit: 'cover',
  },
  icon: {
    width: bar.mediaIconSize,
    height: bar.mediaIconSize,
  },
  label: {
    maxWidth: {
      default: bar.mediaLabelMaxWidth,
      [media.compact]: bar.mediaLabelMaxWidthCompact,
    },
  },
});
