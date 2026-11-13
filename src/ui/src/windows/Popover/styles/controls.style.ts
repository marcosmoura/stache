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

/*
 * The popover's native macOS controls (part 05), shared by its tabs: push
 * buttons, small borderless icon buttons and NSSwitch-like switches. Colors
 * are the `native` system-color tokens only.
 */
export const styles = stylex.create({
  /** A push button: a rounded rectangle with the control fill. */
  button: {
    ...control,
    height: native.buttonHeight,
    paddingBlock: 0,
    paddingInline: space.md,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    fontFamily: 'inherit',
    fontSize: native.fontSize,
    color: native.label,
    appearance: 'none',
    backgroundColor: { default: native.control, ':hover:not(:disabled)': native.controlHover },
    boxShadow: `inset 0 0 0 0.5px ${native.separator}`,
  },
  /** The default push button: the accent fill. */
  primary: {
    color: native.onAccent,
    backgroundColor: { default: native.accent, ':hover:not(:disabled)': native.accent },
  },
  /** A small borderless icon button (retry, pause, reset, stepper). */
  round: {
    ...control,
    display: 'grid',
    placeItems: 'center',
    width: native.buttonHeight,
    height: native.buttonHeight,
    padding: 0,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    color: native.secondaryLabel,
    appearance: 'none',
    backgroundColor: { default: 'transparent', ':hover:not(:disabled)': native.tabHover },
  },
  /** NSSwitch, small size: a gray track, the accent when on, a white knob. */
  switch: {
    ...control,
    position: 'relative',
    flexShrink: 0,
    width: native.switchWidth,
    height: native.switchHeight,
    padding: 0,
    borderStyle: 'none',
    borderRadius: '999px',
    appearance: 'none',
    backgroundColor: native.switchOff,
  },
  switchOn: { backgroundColor: native.accent },
  knob: {
    position: 'absolute',
    top: '1px',
    left: '1px',
    width: `calc(${native.switchHeight} - 2px)`,
    height: `calc(${native.switchHeight} - 2px)`,
    borderRadius: '50%',
    backgroundColor: '#fff',
    boxShadow: '0 0 0 0.5px rgb(0 0 0 / 0.12), 0 1px 1.5px rgb(0 0 0 / 0.2)',
    transitionTimingFunction: easing.standard,
    transitionDuration: { default: duration.normal, [media.reducedMotion]: '0ms' },
    transitionProperty: 'transform',
  },
  knobOn: { transform: `translateX(calc(${native.switchWidth} - ${native.switchHeight}))` },
});
