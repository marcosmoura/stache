import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/*
 * A file's tab, native macOS style (part 05): the path header and the
 * file's state notes above the settings form. Colors are the `native`
 * system-color tokens only.
 */
export const styles = stylex.create({
  /** The "Reading…" line while the document loads. */
  status: { margin: 0, color: native.secondaryLabel },
  error: {
    margin: 0,
    color: native.red,
    overflowWrap: 'anywhere',
  },
  root: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.lg,
    paddingTop: space.sm,
  },
  header: {
    display: 'flex',
    flexWrap: 'wrap',
    gap: space.sm,
    alignItems: 'center',
  },
  path: {
    userSelect: 'text',
    overflow: 'hidden',
    flexBasis: '100%',
    flexGrow: 1,
    minWidth: 0,
    fontFamily: native.familyMono,
    fontSize: native.smallSize,
    color: native.secondaryLabel,
    textOverflow: 'ellipsis',
    whiteSpace: 'nowrap',
  },
  note: {
    margin: 0,
    paddingBlock: space.sm,
    paddingInline: space.md,
    borderRadius: native.cardRadius,
    fontSize: native.smallSize,
    color: native.secondaryLabel,
    overflowWrap: 'anywhere',
    backgroundColor: native.card,
  },
});
