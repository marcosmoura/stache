import * as stylex from '@stylexjs/stylex';

import { duration, easing, guide, media, native, space } from '@/theme/tokens.stylex';

/** A native disclosure: a secondary-label summary with a turning chevron. */
export const styles = stylex.create({
  root: {
    color: native.secondaryLabel,
  },
  summary: {
    cursor: 'default',
    display: 'flex',
    gap: space.xs,
    alignItems: 'center',
    width: 'fit-content',
    padding: 0,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    font: 'inherit',
    fontWeight: native.titleWeight,
    color: native.secondaryLabel,
    appearance: 'none',
    backgroundColor: 'transparent',
    outline: {
      default: 'none',
      ':focus-visible': `3px solid ${native.focus}`,
    },
    outlineOffset: '1px',
  },
  chevron: {
    transform: 'rotate(0deg)',
    display: 'grid',
    placeItems: 'center',
    transitionTimingFunction: easing.standard,
    transitionDuration: { default: duration.fast, [media.reducedMotion]: '0ms' },
    transitionProperty: 'transform',
  },
  chevronOpen: {
    transform: 'rotate(90deg)',
  },
  text: {
    userSelect: 'text',
    marginInline: 0,
    marginTop: space.sm,
    marginBottom: 0,
    paddingLeft: guide.disclosureIndent,
    fontSize: native.smallSize,
    lineHeight: '14px',
  },
});
