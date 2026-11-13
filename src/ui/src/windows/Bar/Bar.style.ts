import * as stylex from '@stylexjs/stylex';

import { bar, color, font, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  /**
   * The Bar window's root rules, on the Bar's own element (`main.css` stays
   * generic): it fills the window, clips the slide, sets the Bar text style and
   * forbids text selection and dragging.
   */
  window: {
    userSelect: 'none',
    position: 'fixed',
    inset: 0,
    overflow: 'hidden',
    boxSizing: 'border-box',
    height: '100%',
    margin: 0,
    fontFamily: font.family,
    fontSize: bar.fontSize,
    fontWeight: bar.fontWeight,
    fontVariantNumeric: 'tabular-nums',
    lineHeight: bar.lineHeight,
    color: color.text,
    textRendering: 'optimizeLegibility',
  },
  root: {
    willChange: 'transform, opacity',
    overflow: 'hidden',
    display: 'grid',
    gridTemplateColumns: 'minmax(max-content, 1fr) minmax(0, auto) minmax(max-content, 1fr)',
    // The side zones never shrink below their content; the center zone gives
    // way first (its scrolling label shrinks). While both sides fit in their
    // equal shares the center zone is centered on the Bar.
    gridTemplateRows: 'minmax(0, 1fr)',
    columnGap: space.lg,
    alignItems: 'center',
    boxSizing: 'border-box',
    width: '100%',
    height: '100vh',
    margin: 0,
  },
  /** The accepted `height` sets the row. */
  sized: (height: number) => ({
    height,
  }),
  zone: {
    display: 'flex',
    gap: space.xs,
    alignItems: 'center',
    minWidth: 0,
    height: '100%',
  },
  start: {
    justifyContent: 'flex-start',
  },
  center: {
    justifyContent: 'center',
  },
  end: {
    justifyContent: 'flex-end',
  },
  slot: {
    display: 'flex',
    flexShrink: 0,
    alignItems: 'center',
    minWidth: 0,
    height: '100%',
  },
  /** Items of the center zone may shrink, so a long label scrolls instead of pushing the sides. */
  shrinkable: {
    flexShrink: 1,
  },
});
