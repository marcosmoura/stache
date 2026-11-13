import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/*
 * The Overview, native macOS style (part 05): translucent inset groups of
 * ~28px rows under small section headings. Colors are the `native`
 * system-color tokens only.
 */
export const styles = stylex.create({
  root: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.lg,
    paddingTop: space.sm,
  },
  muted: { margin: 0, color: native.secondaryLabel },
  error: {
    margin: 0,
    color: native.red,
    overflowWrap: 'anywhere',
  },
  heading: {
    margin: 0,
    marginBottom: space.xs,
    paddingInline: space.sm,
    fontSize: native.sectionSize,
    fontWeight: native.titleWeight,
    color: native.secondaryLabel,
  },
  /** A translucent inset group of rows. */
  card: {
    display: 'flex',
    flexDirection: 'column',
    margin: 0,
    padding: space.xxs,
    borderRadius: native.cardRadius,
    listStyle: 'none',
    backgroundColor: native.card,
  },
});
