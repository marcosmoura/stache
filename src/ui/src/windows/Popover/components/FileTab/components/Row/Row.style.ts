import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/** A ~28px row: the label (and hint) on the left, its controls on the right. */
export const styles = stylex.create({
  row: {
    display: 'grid',
    gridTemplateColumns: 'minmax(0, 1fr) auto',
    rowGap: space.xxs,
    columnGap: space.md,
    alignItems: 'center',
    minHeight: native.rowHeight,
    paddingBlock: space.xs,
    paddingInline: space.sm,
    borderRadius: native.tileRadius,
  },
  rowPending: { opacity: 0.6 },
  text: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xxs,
    minWidth: 0,
  },
  labelLine: {
    display: 'flex',
    gap: space.sm,
    alignItems: 'center',
    minWidth: 0,
  },
  label: {
    overflow: 'hidden',
    fontWeight: native.fontWeight,
    color: native.label,
    textOverflow: 'ellipsis',
    whiteSpace: 'nowrap',
  },
  /** A hint wraps; the copy's length cap keeps it short. */
  help: {
    fontSize: native.smallSize,
    color: native.secondaryLabel,
  },
  controls: {
    display: 'flex',
    gap: space.xs,
    alignItems: 'center',
    justifyContent: 'flex-end',
    minWidth: 0,
  },
});
