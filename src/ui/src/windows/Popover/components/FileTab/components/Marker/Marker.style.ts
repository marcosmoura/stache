import * as stylex from '@stylexjs/stylex';

import { native } from '@/theme/tokens.stylex';

/** A small tertiary note after a label ("default", a count). */
export const styles = stylex.create({
  marker: {
    flexShrink: 0,
    fontSize: native.smallSize,
    color: native.tertiaryLabel,
  },
});
