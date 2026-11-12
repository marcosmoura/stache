import * as stylex from '@stylexjs/stylex';

import { color, font } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  root: {
    display: 'grid',
    placeItems: 'center',
    minHeight: '100vh',
    margin: 0,
    fontFamily: font.family,
    fontSize: font.sizeMd,
    color: color.text,
    backgroundColor: color.surface,
  },
});
