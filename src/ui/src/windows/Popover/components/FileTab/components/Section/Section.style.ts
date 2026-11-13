import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/** A collapsible group: an 11px semibold heading in the secondary label color. */
export const styles = stylex.create({
  section: { display: 'flex', flexDirection: 'column', gap: space.xs },
  sectionHead: {
    display: 'flex',
    gap: space.sm,
    alignItems: 'center',
    minWidth: 0,
  },
  sectionHint: {
    margin: 0,
    fontSize: native.smallSize,
    color: native.secondaryLabel,
  },
});
