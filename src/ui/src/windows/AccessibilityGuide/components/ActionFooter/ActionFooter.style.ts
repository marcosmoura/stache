import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, native, space } from '@/theme/tokens.stylex';

const spin = stylex.keyframes({
  to: { transform: 'rotate(360deg)' },
});

export const styles = stylex.create({
  footer: {
    display: 'grid',
    gridTemplateColumns: '1fr 1fr',
    gap: space.md,
    paddingInline: space.xl,
    paddingTop: space.lg,
    paddingBottom: space.xl,
    borderTopColor: native.separator,
    borderTopStyle: 'solid',
    borderTopWidth: '0.5px',
  },
  /** A macOS push button. */
  button: {
    cursor: 'default',
    display: 'flex',
    gap: space.sm,
    alignItems: 'center',
    justifyContent: 'center',
    height: native.buttonHeight,
    paddingBlock: 0,
    paddingInline: space.lg,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    font: 'inherit',
    color: { default: native.label, ':disabled': native.tertiaryLabel },
    whiteSpace: 'nowrap',
    appearance: 'none',
    backgroundColor: {
      default: native.control,
      ':hover:not(:disabled)': native.controlHover,
    },
    outline: {
      default: 'none',
      ':focus-visible': `3px solid ${native.focus}`,
    },
    outlineOffset: '0px',
    boxShadow: `0 0.5px 1px color-mix(in srgb, ${native.label} 12%, transparent)`,
    transitionTimingFunction: easing.standard,
    transitionDuration: { default: duration.fast, [media.reducedMotion]: '0ms' },
    transitionProperty: 'background-color, filter',
  },
  /** The default button: the user's accent, spanning the footer. */
  primary: {
    gridColumnEnd: '-1',
    gridColumnStart: '1',
    color: native.onAccent,
    opacity: { default: 1, ':disabled': 0.6 },
    backgroundColor: { default: native.accent, ':hover:not(:disabled)': native.accent },
    filter: { default: 'none', ':active:not(:disabled)': 'brightness(0.9)' },
  },
  spinner: {
    display: 'grid',
    placeItems: 'center',
    animationName: { default: spin, [media.reducedMotion]: 'none' },
    animationDuration: duration.spinFast,
    animationTimingFunction: 'linear',
    animationIterationCount: 'infinite',
  },
});
