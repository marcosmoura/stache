import * as stylex from '@stylexjs/stylex';

import { color, font, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  path: {
    marginInline: 0,
    marginTop: space.md,
    marginBottom: 0,
    fontFamily: font.familySystemMono,
    fontSize: font.sizeSm,
    color: color.textMuted,
    overflowWrap: 'anywhere',
  },
});
