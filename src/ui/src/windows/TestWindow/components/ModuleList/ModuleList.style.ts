import * as stylex from '@stylexjs/stylex';

import { color, font, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  meta: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xs,
    marginInline: 0,
    marginTop: space.lg,
    marginBottom: 0,
  },
  row: {
    display: 'grid',
    gridTemplateColumns: 'max-content 1fr',
    gap: space.lg,
  },
  term: {
    fontSize: font.sizeSm,
    color: color.textMuted,
  },
  value: {
    margin: 0,
    fontSize: font.sizeMd,
    color: color.textSecondary,
  },
  code: {
    fontFamily: font.familySystemMono,
  },
});
