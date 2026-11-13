import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/** The invalid-file banner: a tinted group with a red title. */
export const styles = stylex.create({
  alert: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.sm,
    margin: 0,
    padding: space.md,
    borderRadius: native.cardRadius,
    overflowWrap: 'anywhere',
    backgroundColor: native.banner,
  },
  alertTitle: { margin: 0, fontWeight: native.titleWeight, color: native.red },
  list: { margin: 0, paddingInlineStart: space.lg },
  muted: { color: native.secondaryLabel },
  /** The raw parser text, behind "Details". */
  details: {
    fontSize: native.smallSize,
    color: native.secondaryLabel,
  },
  summary: { cursor: 'default', width: 'fit-content' },
  raw: {
    userSelect: 'text',
    display: 'block',
    marginTop: space.xs,
    fontFamily: native.familyMono,
    overflowWrap: 'anywhere',
  },
});
