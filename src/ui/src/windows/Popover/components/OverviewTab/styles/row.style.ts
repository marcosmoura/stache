import * as stylex from '@stylexjs/stylex';

import { native, popover, space } from '@/theme/tokens.stylex';

/** A row of an Overview group: a monochrome icon tile, a name and secondary lines. */
export const styles = stylex.create({
  row: {
    display: 'grid',
    gridTemplateColumns: `${popover.tileSize} minmax(0, 1fr) auto`,
    gap: space.md,
    alignItems: 'center',
    minHeight: native.rowHeight,
    paddingBlock: space.xs,
    paddingInline: space.sm,
  },
  /** The row's monochrome icon. */
  tile: {
    display: 'grid',
    placeItems: 'center',
    alignSelf: 'start',
    width: popover.tileSize,
    height: popover.tileSize,
    color: native.secondaryLabel,
  },
  text: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xxs,
    minWidth: 0,
  },
  name: {
    overflow: 'hidden',
    margin: 0,
    padding: 0,
    fontFamily: 'inherit',
    fontSize: 'inherit',
    fontWeight: native.fontWeight,
    color: native.label,
    textOverflow: 'ellipsis',
    whiteSpace: 'nowrap',
  },
  /** A secondary line; long text wraps, the full text is the tooltip. */
  secondary: {
    WebkitBoxOrient: 'vertical',
    WebkitLineClamp: 3,
    overflow: 'hidden',
    display: '-webkit-box',
    fontSize: native.smallSize,
    color: native.secondaryLabel,
    overflowWrap: 'anywhere',
  },
  danger: { color: native.red },
  warning: { color: native.orange },
  attention: { color: native.orange },
});
