import * as stylex from '@stylexjs/stylex';

import { media, popover, space, zIndex } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  /** The panel: transparent, so the native material shows through. */
  panel: {
    userSelect: 'none',
    position: 'relative',
    overflow: 'hidden',
    display: 'grid',
    gridTemplateRows: 'auto minmax(0, 1fr)',
    boxSizing: 'border-box',
    width: '100%',
    height: '100vh',
    margin: 0,
    borderRadius: popover.panelRadius,
    fontFamily: popover.family,
    fontSize: popover.fontSize,
    fontWeight: popover.fontWeight,
    fontVariantNumeric: 'tabular-nums',
    WebkitFontSmoothing: 'antialiased',
    lineHeight: popover.lineHeight,
    color: popover.label,
    backgroundColor: { default: popover.tint, [media.reducedTransparency]: popover.tintOpaque },
    // The ring sits above the content, so scrolled content never covers it.
    '::after': {
      pointerEvents: 'none',
      content: "''",
      position: 'absolute',
      zIndex: zIndex.raised,
      inset: 0,
      borderRadius: popover.panelRadius,
      boxShadow: `inset 0 0 0 0.5px ${popover.separator}`,
    },
  },
  header: {
    display: 'flex',
    gap: space.md,
    alignItems: 'center',
    height: popover.headerHeight,
    paddingInline: space.lg,
  },
  glyph: {
    display: 'grid',
    placeItems: 'center',
    width: popover.tabSize,
    height: popover.tabSize,
    color: popover.secondaryLabel,
  },
  title: {
    margin: 0,
    fontSize: popover.titleSize,
    fontWeight: popover.titleWeight,
    color: popover.label,
  },
  body: {
    display: 'grid',
    gridTemplateColumns: `${popover.sidebarWidth} minmax(0, 1fr)`,
    minHeight: 0,
  },
  pane: {
    overflowY: 'auto',
    minWidth: 0,
    paddingBlock: space.xs,
    paddingInline: space.lg,
    outline: { default: 'none', ':focus-visible': `3px solid ${popover.focus}` },
    outlineOffset: '-3px',
  },
  paneTitle: {
    margin: 0,
    fontSize: popover.titleSize,
    fontWeight: popover.titleWeight,
    color: popover.label,
  },
});
