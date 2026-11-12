import * as stylex from '@stylexjs/stylex';

import { size } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  root: {
    display: 'inline-block',
    flexShrink: 0,
    width: size.iconMd,
    height: size.iconMd,
    verticalAlign: 'middle',
  },
  sized: (px: number) => ({
    width: px,
    height: px,
  }),
});
