import * as stylex from '@stylexjs/stylex';

import { native } from '@/theme/tokens.stylex';

/** A value the form cannot edit, as TOML-like text. */
export const styles = stylex.create({
  code: {
    userSelect: 'text',
    overflow: 'hidden',
    maxWidth: '180px',
    fontFamily: 'inherit',
    fontSize: native.smallSize,
    color: native.secondaryLabel,
    textOverflow: 'ellipsis',
    whiteSpace: 'nowrap',
  },
});
