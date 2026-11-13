import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/** The Accessibility banner: a tinted group with a title, a line of text and push buttons. */
export const styles = stylex.create({
  banner: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.sm,
    padding: space.md,
    borderRadius: native.cardRadius,
    backgroundColor: native.banner,
  },
  bannerHead: {
    display: 'flex',
    gap: space.sm,
    alignItems: 'center',
    color: native.orange,
  },
  bannerTitle: {
    margin: 0,
    fontSize: 'inherit',
    fontWeight: native.titleWeight,
    color: native.label,
  },
  bannerText: { margin: 0, color: native.secondaryLabel },
  bannerActions: {
    display: 'flex',
    flexWrap: 'wrap',
    gap: space.sm,
  },
});
