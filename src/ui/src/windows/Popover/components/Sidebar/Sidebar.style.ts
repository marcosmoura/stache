import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, native, popover, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  sidebar: {
    scrollbarWidth: 'none',
    overflowY: 'auto',
    width: popover.sidebarWidth,
    paddingBlock: space.md,
    borderInlineEndColor: native.separator,
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
    color: native.secondaryLabel,
    appearance: 'none',
    backgroundColor: { default: 'transparent', ':hover': native.tabHover },
    outline: { default: 'none', ':focus-visible': `3px solid ${native.focus}` },
    outlineOffset: '0px',
    transitionTimingFunction: easing.inOut,
    transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
    transitionProperty: 'background-color, color',
  },
  selected: {
    color: native.onAccent,
    backgroundColor: { default: native.accent, ':hover': native.accent },
  },
  badge: {
    position: 'absolute',
    top: space.xxs,
    right: space.xxs,
    width: popover.badgeSize,
    height: popover.badgeSize,
    borderRadius: '50%',
    boxShadow: `0 0 0 1.5px ${native.menu}`,
  },
  invalid: {
    backgroundColor: native.red,
  },
  diagnostic: {
    backgroundColor: native.orange,
  },
  warnings: {
    backgroundColor: native.yellow,
  },
});
