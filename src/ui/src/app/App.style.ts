import * as stylex from '@stylexjs/stylex';

import { color, font, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  fallback: {
    display: 'grid',
    gap: space.xs,
    placeItems: 'center',
    alignContent: 'center',
    minHeight: '100vh',
    margin: 0,
    fontFamily: font.family,
    fontSize: font.sizeMd,
    color: color.text,
  },
  title: {
    margin: 0,
    fontSize: font.sizeLg,
  },
  detail: {
    margin: 0,
  },
});
