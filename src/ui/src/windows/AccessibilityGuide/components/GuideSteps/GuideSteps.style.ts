import * as stylex from '@stylexjs/stylex';

import { color, font, guide, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  list: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.md,
    margin: 0,
    padding: 0,
    listStyle: 'none',
  },
  step: {
    display: 'flex',
    gap: space.lg,
    alignItems: 'flex-start',
    fontSize: guide.stepSize,
    lineHeight: guide.stepLineHeight,
    color: color.textSecondary,
  },
  number: {
    display: 'grid',
    flexShrink: 0,
    placeItems: 'center',
    width: guide.markSize,
    height: guide.markSize,
    borderRadius: '50%',
    fontSize: font.sizeMd,
    color: color.accent,
    backgroundColor: color.divider,
  },
  strong: {
    fontWeight: guide.titleWeight,
    color: color.text,
  },
});
