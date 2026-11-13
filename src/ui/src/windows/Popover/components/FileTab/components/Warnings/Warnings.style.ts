import * as stylex from '@stylexjs/stylex';

import { native, space } from '@/theme/tokens.stylex';

/** The file's warnings: a tinted group with an orange title. */
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
  warningTitle: { margin: 0, fontWeight: native.titleWeight, color: native.orange },
  list: { margin: 0, paddingInlineStart: space.lg },
});
