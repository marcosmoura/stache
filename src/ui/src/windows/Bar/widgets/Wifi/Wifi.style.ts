import * as stylex from '@stylexjs/stylex';

import { bar, media } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  label: {
    maxWidth: {
      default: bar.labelMaxWidth,
      [media.compact]: bar.labelMaxWidthCompact,
    },
  },
});
