import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, native } from '@/theme/tokens.stylex';

const control = {
  cursor: 'default',
  opacity: { default: 1, ':disabled': 0.4 },
  outline: { default: 'none', ':focus-visible': `3px solid ${native.focus}` },
  outlineOffset: '0px',
  transitionTimingFunction: easing.inOut,
  transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
  transitionProperty: 'background-color, color, opacity, outline-color',
} as const;

export const styles = stylex.create({
  /** A small NSStepper: two stacked arrow buttons beside the field. */
  stepper: {
    overflow: 'hidden',
    display: 'flex',
    flexDirection: 'column',
    flexShrink: 0,
    width: '13px',
    height: native.buttonHeight,
    borderRadius: native.buttonRadius,
    backgroundColor: native.control,
    boxShadow: `inset 0 0 0 0.5px ${native.separator}`,
  },
  step: {
    ...control,
    display: 'grid',
    flexGrow: 1,
    placeItems: 'center',
    padding: 0,
    borderStyle: 'none',
    color: native.label,
    appearance: 'none',
    backgroundColor: { default: 'transparent', ':active:not(:disabled)': native.controlHover },
  },
});
