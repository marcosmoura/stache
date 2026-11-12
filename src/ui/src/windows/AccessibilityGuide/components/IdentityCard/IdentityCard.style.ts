import * as stylex from '@stylexjs/stylex';

import { color, font, guide, space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  card: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.md,
    paddingBlock: space.md,
    paddingInline: space.lg,
    borderRadius: guide.controlRadius,
    backgroundColor: color.surfaceRaised,
    boxShadow: `inset 0 0 0 1px ${color.divider}`,
  },
  fields: {
    userSelect: 'text',
    display: 'flex',
    flexDirection: 'column',
    gap: space.xs,
    margin: 0,
  },
  field: {
    display: 'grid',
    gridTemplateColumns: `${guide.fieldLabelWidth} minmax(0, 1fr)`,
    gap: space.lg,
    fontSize: font.sizeMd,
  },
  term: {
    color: color.textSubtle,
  },
  value: {
    margin: 0,
    color: color.text,
    overflowWrap: 'anywhere',
  },
  hint: {
    display: 'flex',
    gap: space.md,
    alignItems: 'flex-start',
    margin: 0,
    paddingTop: space.md,
    borderTopColor: color.divider,
    borderTopStyle: 'solid',
    borderTopWidth: '1px',
    fontSize: font.sizeMd,
    color: color.warning,
  },
  hintIcon: {
    display: 'flex',
    flexShrink: 0,
    marginTop: '3px',
  },
});
