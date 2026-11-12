import * as stylex from '@stylexjs/stylex';

export const styles = stylex.create({
  /** Clips the item while it enters or leaves (width and opacity). */
  presence: {
    overflow: 'hidden',
    display: 'flex',
    flexShrink: 0,
    height: '100%',
  },
  label: {
    overflow: 'hidden',
    display: 'inline-block',
    whiteSpace: 'nowrap',
  },
});
