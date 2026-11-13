import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, native, space } from '@/theme/tokens.stylex';

/** Shared by every control: the system focus ring and a dimmed disabled state. */
const control = {
  cursor: { default: 'default', ':disabled': 'default' },
  opacity: { default: 1, ':disabled': 0.4 },
  outline: { default: 'none', ':focus-visible': `3px solid ${native.focus}` },
  outlineOffset: '0px',
  transitionTimingFunction: easing.inOut,
  transitionDuration: { default: duration.fast, [media.reducedMotion]: '0ms' },
  transitionProperty: 'background-color, color, opacity',
} as const;

/** The footer: menu-like actions in an inset group. */
export const styles = stylex.create({
  footer: {
    display: 'flex',
    flexDirection: 'column',
    padding: space.xxs,
    borderRadius: native.cardRadius,
    backgroundColor: native.card,
  },
  /** A menu-like action: highlighted in the accent on hover. */
  action: {
    ...control,
    display: 'flex',
    gap: space.md,
    alignItems: 'center',
    minHeight: native.rowHeight,
    paddingBlock: 0,
    paddingInline: space.sm,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    fontFamily: 'inherit',
    fontSize: 'inherit',
    fontWeight: 'inherit',
    color: { default: native.label, ':hover:not(:disabled)': native.onAccent },
    textAlign: 'start',
    appearance: 'none',
    backgroundColor: { default: 'transparent', ':hover:not(:disabled)': native.accent },
  },
});
