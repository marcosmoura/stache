/**
 * Catppuccin Mocha values mirrored from the legacy design system
 * (`_legacy/app/ui/design-system/colors.ts`). Local UI code reads these tokens
 * instead of importing the archive or hard-coding hex values.
 */
export const colors = {
  rosewater: '#f5e0dc',
  flamingo: '#f2cdcd',
  pink: '#f5c2e7',
  mauve: '#cba6f7',
  red: '#f38ba8',
  maroon: '#eba0ac',
  peach: '#fab387',
  yellow: '#f9e2af',
  green: '#a6e3a1',
  teal: '#94e2d5',
  sky: '#89dceb',
  sapphire: '#74c7ec',
  blue: '#89b4fa',
  lavender: '#b4befe',
  text: '#cdd6f4',
  subtext1: '#bac2de',
  subtext0: '#a6adc8',
  overlay2: '#9399b2',
  overlay1: '#7f849c',
  overlay0: '#6c7086',
  surface2: '#585b70',
  surface1: '#45475a',
  surface0: '#313244',
  base: '#1e1e2e',
  mantle: '#181825',
  crust: '#11111b',
} as const;

export const fonts = {
  ui: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", system-ui, sans-serif',
  mono: 'ui-monospace, "SF Mono", Menlo, monospace',
  /** The bar's root stack. System-installed only; never bundled. */
  bar: "'Maple Mono NF', 'SF Mono', monospace",
} as const;

export const space = {
  xs: '4px',
  sm: '8px',
  md: '12px',
  lg: '16px',
  xl: '24px',
  xxl: '32px',
} as const;

export const radii = {
  sm: '4px',
  md: '8px',
  /** Controls, as on legacy buttons. */
  control: '12px',
  /** Outer panels, as on legacy widgets. */
  panel: '16px',
} as const;

/** Mirrors `_legacy/app/ui/design-system/motion.ts`. */
export const motion = {
  easing: 'cubic-bezier(0.42, 0, 0.58, 1)',
  duration: '150ms',
  durationSlow: '400ms',
} as const;
