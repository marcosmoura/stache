import * as stylex from '@stylexjs/stylex';

import { bar, color, duration, easing, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  root: {
    overflow: 'hidden',
    display: 'inline-grid',
    gridAutoFlow: 'column',
    columnGap: space.sm,
    alignItems: 'center',
    boxSizing: 'border-box',
    minWidth: 0,
    maxWidth: '100%',
    height: '100%',
    margin: 0,
    paddingBlock: 0,
    paddingInline: bar.itemPaddingInline,
    borderColor: 'transparent',
    borderStyle: 'solid',
    borderWidth: bar.itemBorderWidth,
    borderRadius: bar.itemRadius,
    font: 'inherit',
    color: 'inherit',
    whiteSpace: 'nowrap',
    backgroundColor: color.surfaceSunken,
    outline: {
      default: 'none',
      ':focus-visible': `2px solid ${color.accent}`,
    },
    outlineOffset: '-2px',
    transitionTimingFunction: easing.inOut,
    transitionDuration: duration.bar,
    transitionProperty: 'background-color, border-color',
  },
  /** A clickable item: the hover look. */
  button: {
    cursor: 'pointer',
    borderColor: {
      default: 'transparent',
      ':hover': color.surface,
    },
    appearance: 'none',
    backgroundColor: {
      default: color.surfaceSunken,
      ':hover': color.surfaceActive,
    },
  },
  active: {
    borderColor: color.surface,
    backgroundColor: color.surfaceActive,
  },
});
