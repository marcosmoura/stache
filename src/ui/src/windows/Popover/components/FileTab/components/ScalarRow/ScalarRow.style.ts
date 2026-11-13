import * as stylex from '@stylexjs/stylex';

import { native } from '@/theme/tokens.stylex';

/** A number field and the unit after its stepper. */
export const styles = stylex.create({
  number: { width: '56px', fontVariantNumeric: 'tabular-nums', textAlign: 'end' },
  /** A number's unit, after its stepper. */
  unit: {
    flexShrink: 0,
    fontSize: native.smallSize,
    color: native.secondaryLabel,
  },
});
