import * as stylex from '@stylexjs/stylex';

import { duration, easing, guide, media, native, space, zIndex } from '@/theme/tokens.stylex';

const panelIn = stylex.keyframes({
  from: { transform: 'scale(0.98)', opacity: 0 },
  to: { transform: 'scale(1)', opacity: 1 },
});

const fadeIn = stylex.keyframes({
  from: { opacity: 0 },
  to: { opacity: 1 },
});

/*
 * The guide has the native macOS look of the popover: a transparent page over
 * the window's rounded native material, system colours and the system font.
 * Colors are the `native` system-color tokens only.
 */
export const styles = stylex.create({
  /** One rounded panel filling the transparent window. */
  panel: {
    userSelect: 'none',
    position: 'relative',
    display: 'grid',
    gridTemplateRows: 'auto minmax(0, 1fr) auto auto',
    boxSizing: 'border-box',
    width: '100%',
    height: '100vh',
    margin: 0,
    borderRadius: native.panelRadius,
    fontFamily: native.family,
    fontSize: native.fontSize,
    fontWeight: native.fontWeight,
    fontVariantNumeric: 'tabular-nums',
    WebkitFontSmoothing: 'antialiased',
    lineHeight: native.lineHeight,
    color: native.label,
    backgroundColor: { default: native.tint, [media.reducedTransparency]: native.tintOpaque },
    animationName: { default: panelIn, [media.reducedMotion]: 'none' },
    animationDuration: duration.normal,
    animationTimingFunction: easing.enter,
    animationFillMode: 'both',
    // The ring sits above the content, so scrolled text never covers it.
    '::after': {
      pointerEvents: 'none',
      content: "''",
      position: 'absolute',
      zIndex: zIndex.raised,
      inset: 0,
      borderRadius: native.panelRadius,
      boxShadow: `inset 0 0 0 0.5px ${native.separator}`,
    },
  },
  header: {
    display: 'flex',
    gap: space.md,
    alignItems: 'center',
    height: guide.headerHeight,
    paddingBlock: 0,
    paddingRight: space.md,
    paddingLeft: space.lg,
  },
  dragRegion: {
    display: 'flex',
    flexGrow: 1,
    gap: space.sm,
    alignItems: 'center',
    height: '100%',
  },
  /** Children of the drag region never take the press, so the drag starts. */
  passThrough: {
    pointerEvents: 'none',
  },
  glyph: {
    display: 'grid',
    placeItems: 'center',
    width: guide.markSize,
    height: guide.markSize,
    color: native.secondaryLabel,
  },
  appName: {
    fontWeight: native.titleWeight,
    color: native.label,
  },
  close: {
    cursor: 'default',
    display: 'grid',
    placeItems: 'center',
    width: guide.closeSize,
    height: guide.closeSize,
    padding: 0,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    color: { default: native.secondaryLabel, ':hover': native.label },
    appearance: 'none',
    backgroundColor: { default: 'transparent', ':hover': native.tabHover },
    outline: {
      default: 'none',
      ':focus-visible': `3px solid ${native.focus}`,
    },
    outlineOffset: '0px',
    transitionTimingFunction: easing.standard,
    transitionDuration: { default: duration.fast, [media.reducedMotion]: '0ms' },
    transitionProperty: 'background-color, color',
  },
  scroll: {
    scrollbarWidth: 'thin',
    overflowX: 'hidden',
    overflowY: 'auto',
    overscrollBehavior: 'contain',
    display: 'flex',
    flexDirection: 'column',
    minHeight: 0,
    paddingInline: space.xl,
    paddingTop: space.xs,
    paddingBottom: space.xl,
  },
  content: {
    display: 'flex',
    flexDirection: 'column',
    gap: guide.sectionGap,
    marginBlock: 0,
    animationName: { default: fadeIn, [media.reducedMotion]: 'none' },
    animationDuration: duration.fade,
    animationTimingFunction: easing.standard,
    animationFillMode: 'both',
  },
  /** Short states center their hero; auto margins keep overflow scrollable. */
  centered: {
    marginBlock: 'auto',
  },
  hero: {
    display: 'flex',
    gap: space.lg,
    alignItems: 'flex-start',
    textAlign: 'left',
  },
  heroCentered: {
    flexDirection: 'column',
    alignItems: 'center',
    textAlign: 'center',
  },
  heroText: {
    display: 'flex',
    flexDirection: 'column',
    flexGrow: 1,
    gap: space.xs,
    minWidth: 0,
  },
  title: {
    margin: 0,
    fontSize: guide.titleSize,
    fontWeight: native.titleWeight,
    lineHeight: '20px',
    color: native.label,
  },
  body: {
    margin: 0,
    color: native.secondaryLabel,
  },
  /** A status line: a small coloured dot before the text. */
  status: {
    // Centres the dot on the first line's x-height.
    transform: 'translateY(-1px)',
    display: 'inline-block',
    width: native.dotSize,
    height: native.dotSize,
    marginRight: space.sm,
    borderRadius: '50%',
    verticalAlign: 'middle',
  },
  neutral: { backgroundColor: native.gray },
  accent: { backgroundColor: native.orange },
  attention: { backgroundColor: native.red },
  success: { backgroundColor: native.green },
  alert: {
    display: 'flex',
    gap: space.md,
    alignItems: 'flex-start',
    marginBlock: space.md,
    marginInline: space.xl,
    paddingBlock: space.md,
    paddingInline: space.lg,
    borderRadius: native.cardRadius,
    fontSize: native.smallSize,
    lineHeight: '14px',
    color: native.label,
    overflowWrap: 'anywhere',
    backgroundColor: native.bannerError,
  },
  alertIcon: {
    display: 'flex',
    flexShrink: 0,
    color: native.red,
  },
});
