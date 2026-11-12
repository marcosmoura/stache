import type { Transition } from 'motion/react';

/*
 * Motion values for `motion/react`. `tokens.stylex.ts` holds the same durations
 * and easings as CSS strings (`duration`, `easing`); a test keeps them equal.
 */

/** Durations in seconds. */
export const durations = {
  instant: 0,
  fast: 0.12,
  normal: 0.2,
  slow: 0.32,
  /** Bar slide-away and Bar item hover transitions. */
  bar: 0.15,
  /** Bar item fade-in on mount. */
  fadeIn: 0.275,
  /** The Spaces indicator spring. */
  spring: 0.5,
  /** The Accessibility guide panel's entrance (CSS keyframes). */
  panelIn: 0.4,
  /** The guide's content fade on a state change (CSS keyframes). */
  fade: 0.15,
  /** The guide's checking badge pulse (CSS keyframes). */
  pulse: 1.6,
  /** The guide's checking spinner (CSS keyframes). */
  spin: 1.2,
  /** A pending button's spinner (CSS keyframes). */
  spinFast: 0.9,
} as const;

/** Cubic Bézier easings. */
export const easings = {
  standard: [0.2, 0, 0, 1],
  enter: [0, 0, 0.2, 1],
  exit: [0.4, 0, 1, 1],
  /** The Bar's ease-in-out. */
  inOut: [0.42, 0, 0.58, 1],
} as const satisfies Record<string, readonly [number, number, number, number]>;

/** Named transition presets. */
export const transitions = {
  standard: { duration: durations.normal, ease: easings.standard },
  enter: { duration: durations.normal, ease: easings.enter },
  exit: { duration: durations.fast, ease: easings.exit },
  slow: { duration: durations.slow, ease: easings.standard },
  /** Bar item fade-in on mount (opacity). */
  fadeIn: { duration: durations.fadeIn, ease: easings.inOut },
  /** The Spaces indicator: a spring without bounce. */
  spring: { type: 'spring', bounce: 0, duration: durations.spring },
} as const satisfies Record<string, Transition>;

/**
 * `ScrollingLabel`'s continuous marquee: the track moves left at `speed` px/s;
 * a second copy follows the text `gap` (a fraction of the box width) behind.
 */
export const scrolling = {
  speed: 60,
  gap: 0.5,
} as const;

/** The default transition `MotionProvider` gives every `m.*` component. */
export const defaultTransition: Transition = transitions.standard;
