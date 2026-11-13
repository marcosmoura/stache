import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, popover, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  sidebar: {
    scrollbarWidth: 'none',
    overflowY: 'auto',
    width: popover.sidebarWidth,
    paddingBlock: space.md,
    borderInlineEndColor: popover.separator,
    borderInlineEndStyle: 'solid',
    borderInlineEndWidth: '0.5px',
  },
  list: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xxs,
    alignItems: 'center',
  },
  tab: {
    cursor: 'pointer',
    position: 'relative',
    display: 'grid',
    flexShrink: 0,
    placeItems: 'center',
    width: popover.tabSize,
    height: popover.tabSize,
    padding: 0,
    borderStyle: 'none',
    borderRadius: popover.tabRadius,
    color: popover.secondaryLabel,
    appearance: 'none',
    backgroundColor: { default: 'transparent', ':hover': popover.tabHover },
    outline: { default: 'none', ':focus-visible': `3px solid ${popover.focus}` },
    outlineOffset: '0px',
    transitionTimingFunction: easing.inOut,
    transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
    transitionProperty: 'background-color, color',
  },
  selected: {
    color: popover.onAccent,
    backgroundColor: { default: popover.accent, ':hover': popover.accent },
  },
  badge: {
    position: 'absolute',
    top: space.xxs,
    right: space.xxs,
    width: popover.badgeSize,
    height: popover.badgeSize,
    borderRadius: '50%',
    boxShadow: `0 0 0 1.5px ${popover.menu}`,
  },
  invalid: {
    backgroundColor: popover.red,
  },
  diagnostic: {
    backgroundColor: popover.orange,
  },
  warnings: {
    backgroundColor: popover.yellow,
  },
});
