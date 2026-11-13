import * as stylex from '@stylexjs/stylex';

import { latte, mocha } from './palette.stylex';

/*
 * Semantic tokens: the only values components use. Colors are
 * `light-dark(<latte>, <mocha>)`, so the root `color-scheme: light dark` and the
 * window's native appearance pick the theme with no script. Each group is an
 * extension point: `stylex.createTheme(group, {...})` overrides it for a subtree.
 */

/** Surfaces, text levels, border, accent and status tones. */
export const color = stylex.defineVars({
  surface: `light-dark(${latte.base}, ${mocha.base})`,
  surfaceRaised: `light-dark(${latte.mantle}, ${mocha.mantle})`,
  surfaceSunken: `light-dark(${latte.crust}, ${mocha.crust})`,
  surfaceHover: `light-dark(${latte.surface0}, ${mocha.surface0})`,
  /** A pressed or hovered Bar item (`surface1`). */
  surfaceActive: `light-dark(${latte.surface1}, ${mocha.surface1})`,
  text: `light-dark(${latte.text}, ${mocha.text})`,
  textMuted: `light-dark(${latte.subtext0}, ${mocha.subtext0})`,
  textSubtle: `light-dark(${latte.overlay1}, ${mocha.overlay1})`,
  /** Secondary text between `text` and `textMuted` (`subtext1`). */
  textSecondary: `light-dark(${latte.subtext1}, ${mocha.subtext1})`,
  /** Faint metadata text (`overlay2`). */
  textFaint: `light-dark(${latte.overlay2}, ${mocha.overlay2})`,
  border: `light-dark(${latte.surface1}, ${mocha.surface1})`,
  /** Hairline separators and panel rings (`surface0`). */
  divider: `light-dark(${latte.surface0}, ${mocha.surface0})`,
  /** Keyboard focus outline of window controls (`lavender`). */
  focus: `light-dark(${latte.lavender}, ${mocha.lavender})`,
  accent: `light-dark(${latte.mauve}, ${mocha.mauve})`,
  onAccent: `light-dark(${latte.base}, ${mocha.crust})`,
  success: `light-dark(${latte.green}, ${mocha.green})`,
  warning: `light-dark(${latte.yellow}, ${mocha.yellow})`,
  danger: `light-dark(${latte.red}, ${mocha.red})`,
  /** A softer alert than `danger` (`peach`): revoked access, rejected configuration. */
  attention: `light-dark(${latte.peach}, ${mocha.peach})`,
  info: `light-dark(${latte.blue}, ${mocha.blue})`,
  /** A secondary informational tone (`sky`). */
  highlight: `light-dark(${latte.sky}, ${mocha.sky})`,
});

/** Spacing scale. */
export const space = stylex.defineVars({
  none: '0px',
  xxs: '2px',
  xs: '4px',
  sm: '6px',
  md: '8px',
  lg: '12px',
  xl: '16px',
  xxl: '24px',
  xxxl: '32px',
});

/** Corner radii. */
export const radius = stylex.defineVars({
  none: '0px',
  sm: '4px',
  md: '6px',
  lg: '10px',
  full: '9999px',
});

/** Font family (system installed, never bundled), sizes, weights and line heights. */
export const font = stylex.defineVars({
  family: "'Maple Mono NF', 'SF Mono', monospace",
  /** The system UI stack (the test window). */
  familySystem:
    '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", system-ui, sans-serif',
  /** The system monospace stack (identifiers and paths in the test window). */
  familySystemMono: 'ui-monospace, "SF Mono", Menlo, monospace',
  sizeXs: '10px',
  sizeSm: '11px',
  sizeMd: '12px',
  sizeLg: '14px',
  sizeXl: '16px',
  weightRegular: '400',
  weightMedium: '500',
  weightBold: '700',
  lineHeightTight: '1.2',
  lineHeightNormal: '1.4',
});

/**
 * The Bar's text style and item shape. Item gaps use `space.sm` (inside an
 * item) and `space.xs` (between items); item colors use `color`.
 */
export const bar = stylex.defineVars({
  fontSize: '13px',
  fontWeight: '500',
  lineHeight: '1.6',
  itemRadius: '12px',
  itemPaddingInline: '10px',
  itemBorderWidth: '1px',
  /** Width of `ScrollingLabel`'s faded edges while it scrolls. */
  labelFadeWidth: '10px',
  /** Width and height of a widget icon. */
  iconSize: '18px',
  /** Widest a widget's text label grows before it scrolls. */
  labelMaxWidth: '256px',
  /** `labelMaxWidth` in compact mode. */
  labelMaxWidthCompact: '160px',
  /** Media's player icon (larger than other widget icons, as today). */
  mediaIconSize: '22px',
  /** Media's leading padding while it shows artwork (plus the 1px border: 2px). */
  mediaArtworkInset: '1px',
  /** Media's artwork image (square). */
  mediaArtworkSize: '24px',
  /** Widest Media's label grows before it scrolls. */
  mediaLabelMaxWidth: '560px',
  /** `mediaLabelMaxWidth` in compact mode. */
  mediaLabelMaxWidthCompact: '400px',
  /** The icon opacity of an inactive Spaces workspace. */
  inactiveOpacity: '0.4',
});

/**
 * The Accessibility guide's text style and shape: its own size and weight in
 * the Bar typeface (`font.family`), a 16px panel and 12px controls.
 */
export const guide = stylex.defineVars({
  fontSize: '13px',
  fontWeight: '500',
  lineHeight: '1.6em',
  titleSize: '17px',
  titleWeight: '600',
  titleLineHeight: '1.3',
  stepSize: '12.5px',
  stepLineHeight: '22px',
  panelRadius: '16px',
  controlRadius: '12px',
  headerHeight: '48px',
  /** The app glyph in the header and the step numbers. */
  markSize: '22px',
  closeSize: '28px',
  buttonHeight: '36px',
  /** The hero badge: beside the title, and centered for checking/open. */
  badgeSize: '44px',
  badgeSizeLarge: '64px',
  /** Gap between the body's sections. */
  sectionGap: '14px',
  /** Width of the identity card's labels. */
  fieldLabelWidth: '68px',
  /** Indent of the troubleshooting text under its summary. */
  disclosureIndent: '18px',
});

/** The test window's page: system type, a centered column and card shape. */
export const statusPage = stylex.defineVars({
  fontSize: '13px',
  lineHeight: '1.5',
  titleSize: '20px',
  titleWeight: '600',
  maxWidth: '54rem',
  cardRadius: '8px',
  ruleWidth: '3px',
});

/**
 * The tray popover: a native macOS look (part 05). Colors are WebKit's
 * `-apple-system-*` semantic keywords and the user's accent, so they follow the
 * app appearance and the system settings with no hand-picked dark values;
 * fills are `color-mix` of the label color. Never the Catppuccin palette.
 */
export const popover = stylex.defineVars({
  /** The system UI stack; monospace only for file paths. */
  family: '-apple-system, BlinkMacSystemFont, system-ui, sans-serif',
  familyMono: 'ui-monospace, "SF Mono", Menlo, monospace',
  fontSize: '13px',
  fontWeight: '400',
  lineHeight: '16px',
  titleSize: '13px',
  titleWeight: '600',
  /** The native corner radius of the material behind the webview (panel.rs). */
  panelRadius: '12px',
  headerHeight: '36px',
  sidebarWidth: '44px',
  tabSize: '28px',
  tabRadius: '6px',
  iconSize: '16px',
  badgeSize: '7px',
  smallSize: '11px',
  sectionSize: '11px',
  /** An inset group of rows. */
  cardRadius: '8px',
  rowHeight: '28px',
  tileSize: '22px',
  tileRadius: '5px',
  /** A push button and a stepper button. */
  buttonHeight: '22px',
  buttonRadius: '5px',
  /** NSSwitch (small). */
  switchWidth: '26px',
  switchHeight: '15px',
  dotSize: '7px',
  /** Text levels. */
  label: '-apple-system-label',
  secondaryLabel: '-apple-system-secondary-label',
  tertiaryLabel: '-apple-system-tertiary-label',
  separator: '-apple-system-separator',
  /** The user's accent, the text on it, and the focus ring. */
  accent: '-apple-system-control-accent',
  onAccent: '-apple-system-alternate-selected-text',
  selection: '-apple-system-selected-content-background',
  focus: '-webkit-focus-ring-color',
  /** Status tones: the system red, orange, green and gray. */
  red: '-apple-system-red',
  orange: '-apple-system-orange',
  green: '-apple-system-green',
  gray: '-apple-system-gray',
  yellow: '-apple-system-yellow',
  /** The page stays transparent, so the native material shows. */
  tint: 'transparent',
  /** Reduced transparency: the opaque window background. */
  tintOpaque: '-apple-system-control-background',
  /** A translucent inset group, as in Control Center and System Settings. */
  card: 'color-mix(in srgb, -apple-system-label 5%, transparent)',
  /** A hovered row or tab. */
  tabHover: 'color-mix(in srgb, -apple-system-label 8%, transparent)',
  /** A push button's face and a text field's fill. */
  control: 'color-mix(in srgb, -apple-system-label 9%, transparent)',
  controlHover: 'color-mix(in srgb, -apple-system-label 14%, transparent)',
  /** An off switch track. */
  switchOff: 'color-mix(in srgb, -apple-system-label 12%, transparent)',
  /** A menu's raised surface. */
  menu: '-apple-system-control-background',
  /** The Accessibility banner: a soft orange fill. */
  banner: 'color-mix(in srgb, -apple-system-orange 14%, transparent)',
});

/** Elevation shadows. */
export const shadow = stylex.defineVars({
  none: 'none',
  sm: '0 1px 2px rgb(0 0 0 / 0.12)',
  md: '0 4px 12px rgb(0 0 0 / 0.16)',
  lg: '0 12px 32px rgb(0 0 0 / 0.24)',
});

/** Control and icon sizes. */
export const size = stylex.defineVars({
  controlSm: '20px',
  controlMd: '24px',
  controlLg: '28px',
  iconSm: '12px',
  iconMd: '16px',
  iconLg: '20px',
});

/** Static durations; the same values as `durations` in `motion.ts`. */
export const duration = stylex.defineConsts({
  instant: '0ms',
  fast: '120ms',
  normal: '200ms',
  slow: '320ms',
  bar: '150ms',
  fadeIn: '275ms',
  spring: '500ms',
  flash: '500ms',
  panelIn: '400ms',
  fade: '150ms',
  pulse: '1600ms',
  spin: '1200ms',
  spinFast: '900ms',
});

/** Static easings; the same values as `easings` in `motion.ts`. */
export const easing = stylex.defineConsts({
  standard: 'cubic-bezier(0.2, 0, 0, 1)',
  enter: 'cubic-bezier(0, 0, 0.2, 1)',
  exit: 'cubic-bezier(0.4, 0, 1, 1)',
  inOut: 'cubic-bezier(0.42, 0, 0.58, 1)',
});

/** Stacking order. */
export const zIndex = stylex.defineConsts({
  base: '0',
  raised: '10',
  overlay: '100',
  popover: '200',
  toast: '300',
});

/** Accessibility media queries. */
export const media = stylex.defineConsts({
  reducedMotion: '@media (prefers-reduced-motion: reduce)',
  reducedTransparency: '@media (prefers-reduced-transparency: reduce)',
  moreContrast: '@media (prefers-contrast: more)',
  /** Compact (laptop) Bar rules; `hooks/useMediaQuery` reads the same const. */
  compact: '@media (width <= 2036px)',
});
