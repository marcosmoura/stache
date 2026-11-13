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

/** A module row: its name (a button when it has a file tab), a status dot with the state, and its controls. */
export const styles = stylex.create({
  titleLine: {
    display: 'flex',
    gap: space.sm,
    alignItems: 'center',
    minWidth: 0,
  },
  nameButton: {
    ...control,
    borderStyle: 'none',
    borderRadius: space.xs,
    textAlign: 'start',
    appearance: 'none',
    backgroundColor: 'transparent',
  },
  /** The module state: secondary text after a small status dot. */
  state: {
    display: 'inline-flex',
    flexShrink: 0,
    gap: space.xs,
    alignItems: 'center',
    fontSize: native.smallSize,
    color: native.secondaryLabel,
  },
  dot: {
    flexShrink: 0,
    width: native.dotSize,
    height: native.dotSize,
    borderRadius: '50%',
  },
  green: { backgroundColor: native.green },
  orange: { backgroundColor: native.orange },
  red: { backgroundColor: native.red },
  gray: { backgroundColor: native.gray },
  controls: {
    display: 'flex',
    gap: space.sm,
    alignItems: 'center',
  },
});
