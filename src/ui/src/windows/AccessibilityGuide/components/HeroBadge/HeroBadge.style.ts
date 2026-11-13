import * as stylex from '@stylexjs/stylex';

import { duration, easing, guide, media, native } from '@/theme/tokens.stylex';

const pulse = stylex.keyframes({
  '0%': { opacity: 1 },
  '50%': { opacity: 0.55 },
  '100%': { opacity: 1 },
});

const spin = stylex.keyframes({
  to: { transform: 'rotate(360deg)' },
});

export const styles = stylex.create({
  root: {
    display: 'grid',
    flexShrink: 0,
    placeItems: 'center',
    width: guide.badgeSize,
    height: guide.badgeSize,
    borderRadius: '50%',
    backgroundColor: 'color-mix(in srgb, currentColor 16%, transparent)',
  },
  large: {
    width: guide.badgeSizeLarge,
    height: guide.badgeSizeLarge,
  },
  neutral: {
    color: native.secondaryLabel,
    animationName: { default: pulse, [media.reducedMotion]: 'none' },
    animationDuration: duration.pulse,
    animationTimingFunction: easing.inOut,
    animationIterationCount: 'infinite',
  },
  accent: { color: native.accent },
  attention: { color: native.orange },
  success: { color: native.green },
  spinner: {
    display: 'grid',
    placeItems: 'center',
    animationName: { default: spin, [media.reducedMotion]: 'none' },
    animationDuration: duration.spin,
    animationTimingFunction: 'linear',
    animationIterationCount: 'infinite',
  },
});
