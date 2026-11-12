import * as stylex from '@stylexjs/stylex';

import { bar, color, duration, easing } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  root: {
    cursor: 'pointer',
    position: 'relative',
    zIndex: 1,
    display: 'inline-grid',
    placeItems: 'center',
    boxSizing: 'border-box',
    height: '100%',
    margin: 0,
    paddingBlock: 0,
    paddingInline: bar.itemPaddingInline,
    borderColor: {
      default: 'transparent',
      ':hover': color.surface,
    },
    borderStyle: 'solid',
    borderWidth: bar.itemBorderWidth,
    borderRadius: bar.itemRadius,
    font: 'inherit',
    color: 'inherit',
    appearance: 'none',
    backgroundColor: {
      default: 'transparent',
      ':hover': color.surfaceActive,
    },
    outline: {
      default: 'none',
      ':focus-visible': `2px solid ${color.accent}`,
    },
    outlineOffset: '-2px',
    transitionTimingFunction: easing.inOut,
    transitionDuration: duration.bar,
    transitionProperty: 'background-color, border-color',
  },
  /** The focused workspace's highlight, shared between workspaces through `layoutId`. */
  highlight: {
    position: 'absolute',
    zIndex: -1,
    inset: 0,
    borderRadius: bar.itemRadius,
    backgroundColor: color.surfaceActive,
  },
  /** An inactive workspace: no hover surface, the default cursor. */
  inactive: {
    cursor: 'default',
    borderColor: 'transparent',
    backgroundColor: 'transparent',
  },
  /** An inactive workspace's icon, dimmed. */
  inactiveIcon: {
    opacity: bar.inactiveOpacity,
  },
  /** The refusal flash: an attention tint over the item, faded in and out by Motion. */
  flash: {
    pointerEvents: 'none',
    position: 'absolute',
    zIndex: -1,
    inset: 0,
    borderRadius: bar.itemRadius,
    backgroundColor: color.attention,
  },
});
