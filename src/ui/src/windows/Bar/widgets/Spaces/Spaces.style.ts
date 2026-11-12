import * as stylex from '@stylexjs/stylex';

import { space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  /** The strip's own row: its mount node is boxless, so it lays out its items. */
  row: {
    display: 'flex',
    flexShrink: 0,
    gap: space.xs,
    alignItems: 'center',
    height: '100%',
  },
  /** The workspace list: one item holding the workspace buttons edge to edge. */
  workspaces: {
    columnGap: 0,
    paddingInline: 0,
  },
});
