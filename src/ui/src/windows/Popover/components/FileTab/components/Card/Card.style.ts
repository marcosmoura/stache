import * as stylex from '@stylexjs/stylex';

import { native } from '@/theme/tokens.stylex';

/** A card's drop highlight and drag handle. */
export const styles = stylex.create({
  dropTarget: { boxShadow: `inset 0 0 0 1px ${native.accent}` },
  handle: { cursor: 'grab', flexShrink: 0, color: native.secondaryLabel },
});
