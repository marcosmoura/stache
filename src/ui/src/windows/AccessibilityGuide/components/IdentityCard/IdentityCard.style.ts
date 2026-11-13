import * as stylex from '@stylexjs/stylex';

import { guide, native, space } from '@/theme/tokens.stylex';

/** An inset translucent group of label/value rows, then a hint. */
export const styles = stylex.create({
  card: {
    display: 'flex',
    flexDirection: 'column',
    paddingInline: space.lg,
    borderRadius: native.cardRadius,
    backgroundColor: native.card,
  },
  fields: {
    userSelect: 'text',
    display: 'flex',
    flexDirection: 'column',
    margin: 0,
  },
  field: {
    display: 'grid',
    gridTemplateColumns: `${guide.fieldLabelWidth} minmax(0, 1fr)`,
    gap: space.lg,
    alignItems: 'baseline',
    paddingBlock: space.sm,
    borderBottomColor: native.separator,
    borderBottomStyle: 'solid',
    borderBottomWidth: '0.5px',
  },
  term: {
    color: native.secondaryLabel,
  },
  value: {
    margin: 0,
    color: native.label,
    overflowWrap: 'anywhere',
  },
  hint: {
    display: 'flex',
    gap: space.sm,
    alignItems: 'flex-start',
    margin: 0,
    paddingBlock: space.sm,
    fontSize: native.smallSize,
    lineHeight: '14px',
    color: native.secondaryLabel,
  },
  hintIcon: {
    display: 'flex',
    flexShrink: 0,
    color: native.orange,
  },
});
