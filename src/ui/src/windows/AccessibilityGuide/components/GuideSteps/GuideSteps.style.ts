import * as stylex from '@stylexjs/stylex';

import { guide, native, space } from '@/theme/tokens.stylex';

/** The steps: an inset translucent group with numbered rows. */
export const styles = stylex.create({
  list: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xs,
    margin: 0,
    paddingBlock: space.sm,
    paddingInline: space.lg,
    borderRadius: native.cardRadius,
    listStyle: 'none',
    backgroundColor: native.card,
  },
  step: {
    display: 'flex',
    gap: space.md,
    alignItems: 'flex-start',
    paddingBlock: space.xs,
    color: native.label,
  },
  number: {
    display: 'grid',
    flexShrink: 0,
    placeItems: 'center',
    width: guide.markSize,
    height: guide.markSize,
    marginTop: '-2px',
    borderRadius: '50%',
    fontSize: native.smallSize,
    fontWeight: native.titleWeight,
    color: native.onAccent,
    backgroundColor: native.accent,
  },
  strong: {
    fontWeight: native.titleWeight,
    color: native.label,
  },
});
