import * as stylex from '@stylexjs/stylex';

import { color, duration, easing, guide, media, space } from '@/theme/tokens.stylex';

const spin = stylex.keyframes({
  to: { transform: 'rotate(360deg)' },
});

export const styles = stylex.create({
  footer: {
    display: 'grid',
    gridTemplateColumns: '1fr 1fr',
    gap: space.md,
    paddingInline: space.xxl,
    paddingTop: space.lg,
    paddingBottom: space.xl,
    borderTopColor: color.divider,
    borderTopStyle: 'solid',
    borderTopWidth: '1px',
    backgroundColor: color.surfaceRaised,
  },
  button: {
    cursor: { default: 'pointer', ':disabled': 'default' },
    display: 'flex',
    gap: space.md,
    alignItems: 'center',
    justifyContent: 'center',
    height: guide.buttonHeight,
    paddingBlock: 0,
    paddingInline: space.xl,
    borderColor: { default: 'transparent', ':hover:not(:disabled)': color.surface },
    borderStyle: 'solid',
    borderWidth: '1px',
    borderRadius: guide.controlRadius,
    font: 'inherit',
    color: { default: color.text, ':disabled': color.textMuted },
    whiteSpace: 'nowrap',
    appearance: 'none',
    backgroundColor: {
      default: color.divider,
      ':hover:not(:disabled)': color.surfaceActive,
    },
    outline: {
      default: 'none',
      ':focus-visible': `2px solid ${color.focus}`,
    },
    outlineOffset: '2px',
    transitionTimingFunction: easing.inOut,
    transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
    transitionProperty: 'background-color, border-color',
  },
  primary: {
    gridColumnEnd: '-1',
    gridColumnStart: '1',
    borderColor: 'transparent',
    color: color.onAccent,
    opacity: { default: 1, ':disabled': 0.7 },
    backgroundColor: {
      default: color.accent,
      ':hover:not(:disabled)': `color-mix(in srgb, ${color.accent} 82%, ${color.text})`,
    },
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
