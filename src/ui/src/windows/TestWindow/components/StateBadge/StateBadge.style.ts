import * as stylex from '@stylexjs/stylex';

import { color, font, radius, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  root: {
    display: 'inline-flex',
    flexShrink: 0,
    alignItems: 'center',
    paddingBlock: space.xs,
    paddingInline: space.md,
    borderRadius: radius.sm,
    fontFamily: font.familySystemMono,
    fontSize: font.sizeSm,
    color: color.textSecondary,
    backgroundColor: color.divider,
  },
  neutral: {},
  success: { color: color.success },
  danger: { color: color.danger },
  warning: { color: color.warning },
  attention: { color: color.attention },
});
