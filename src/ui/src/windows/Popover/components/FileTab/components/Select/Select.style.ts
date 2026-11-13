import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, native, space, zIndex } from '@/theme/tokens.stylex';

const control = {
  cursor: 'default',
  opacity: { default: 1, ':disabled': 0.4 },
  outline: { default: 'none', ':focus-visible': `3px solid ${native.focus}` },
  outlineOffset: '0px',
  transitionTimingFunction: easing.inOut,
  transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
  transitionProperty: 'background-color, color, opacity, outline-color',
} as const;

/*
 * A macOS pop-up button (the value plus an up/down chevron) and its menu on
 * the system menu surface, with a check on the selected item.
 */
export const styles = stylex.create({
  /** A pop-up button: the value plus a small up/down chevron. */
  trigger: {
    ...control,
    display: 'flex',
    gap: space.xs,
    alignItems: 'center',
    maxWidth: '180px',
    height: native.buttonHeight,
    paddingBlock: 0,
    paddingInlineStart: space.md,
    paddingInlineEnd: space.xs,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    fontFamily: 'inherit',
    fontSize: 'inherit',
    color: native.label,
    appearance: 'none',
    backgroundColor: { default: native.control, ':hover:not(:disabled)': native.controlHover },
    boxShadow: `inset 0 0 0 0.5px ${native.separator}`,
  },
  triggerChevron: { flexShrink: 0, color: native.secondaryLabel },
  triggerText: { overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' },
  /** The pop-up menu on the system menu surface. */
  menu: {
    position: 'fixed',
    zIndex: zIndex.popover,
    overflowY: 'auto',
    boxSizing: 'border-box',
    width: '260px',
    maxHeight: '280px',
    margin: 0,
    padding: space.xs,
    borderRadius: native.cardRadius,
    listStyle: 'none',
    backgroundColor: native.menu,
    boxShadow: `0 6px 20px rgb(0 0 0 / 0.25), inset 0 0 0 0.5px ${native.separator}`,
  },
  option: {
    cursor: 'default',
    display: 'grid',
    gridTemplateColumns: '14px minmax(0, 1fr)',
    gap: space.xs,
    alignItems: 'center',
    paddingBlock: space.xxs,
    paddingInline: space.xs,
    borderRadius: native.tileRadius,
    color: { default: native.label, ':hover': native.onAccent, ':focus': native.onAccent },
    backgroundColor: { default: 'transparent', ':hover': native.accent, ':focus': native.accent },
    outline: 'none',
  },
  optionText: { display: 'flex', flexDirection: 'column', minWidth: 0 },
  optionTitle: { color: 'inherit' },
  optionHelp: {
    fontSize: native.smallSize,
    color: 'inherit',
    opacity: 0.75,
  },
  check: { display: 'grid', placeItems: 'center', color: 'inherit' },
  place: (top: string, bottom: string, right: string) => ({ top, right, bottom }),
});
