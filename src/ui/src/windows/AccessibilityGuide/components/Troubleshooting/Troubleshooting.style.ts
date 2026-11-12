import * as stylex from '@stylexjs/stylex';

import { color, duration, easing, font, guide, media, radius, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  root: {
    fontSize: font.sizeMd,
    color: color.textMuted,
  },
  summary: {
    cursor: 'pointer',
    display: 'flex',
    gap: space.xs,
    alignItems: 'center',
    width: 'fit-content',
    padding: 0,
    borderStyle: 'none',
    borderRadius: radius.md,
    font: 'inherit',
    color: color.textSecondary,
    appearance: 'none',
    backgroundColor: 'transparent',
    outline: {
      default: 'none',
      ':focus-visible': `2px solid ${color.focus}`,
    },
    outlineOffset: '2px',
  },
  chevron: {
    transform: 'rotate(0deg)',
    display: 'grid',
    placeItems: 'center',
    transitionTimingFunction: easing.inOut,
    transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
    transitionProperty: 'transform',
  },
  chevronOpen: {
    transform: 'rotate(90deg)',
  },
  text: {
    userSelect: 'text',
    marginInline: 0,
    marginTop: space.md,
    marginBottom: 0,
    paddingLeft: guide.disclosureIndent,
  },
});
