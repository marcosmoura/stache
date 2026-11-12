import * as stylex from '@stylexjs/stylex';

import { bar, space } from '@/theme/tokens.stylex';

const fade = bar.labelFadeWidth;

export const styles = stylex.create({
  box: {
    overflow: 'hidden',
    display: 'block',
    minWidth: 0,
    maxWidth: '100%',
    whiteSpace: 'nowrap',
  },
  /** While scrolling the box widens by `space.xs` per side and fades both edges. */
  scrolling: {
    maskImage: `linear-gradient(to right, transparent, black ${fade}, black calc(100% - ${fade}), transparent)`,
    marginInline: `calc(-1 * ${space.xs})`,
  },
  truncated: {
    textOverflow: 'ellipsis',
  },
  text: {
    display: 'inline-block',
  },
  /** The moving track: the text, then (while scrolling) its follow-up copy. */
  track: {
    display: 'inline-block',
  },
  /** The follow-up copy, `gap` px after the text; its text comes from `data-text`. */
  clone: (gap: number) => ({
    marginInlineStart: gap,
    '::before': {
      content: 'attr(data-text)',
    },
  }),
  inline: {
    display: 'inline',
  },
});
