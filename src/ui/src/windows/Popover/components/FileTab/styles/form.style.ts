import * as stylex from '@stylexjs/stylex';

import { duration, easing, media, native, space } from '@/theme/tokens.stylex';

const control = {
  cursor: 'default',
  opacity: { default: 1, ':disabled': 0.4 },
  outline: { default: 'none', ':focus-visible': `3px solid ${native.focus}` },
  outlineOffset: '0px',
  transitionTimingFunction: easing.inOut,
  transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
  transitionProperty: 'background-color, color, opacity, outline-color',
} as const;

/*
 * Styles shared by the settings form's components, native macOS style
 * (part 05): translucent inset groups, filled borderless text fields,
 * disclosure toggles and inline messages. Colors are the `native`
 * system-color tokens only.
 */
export const styles = stylex.create({
  /** An inset card grouping rows. */
  card: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xxs,
    margin: 0,
    padding: space.xs,
    borderRadius: native.cardRadius,
    listStyle: 'none',
    backgroundColor: native.card,
  },
  cards: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xs,
    margin: 0,
    padding: 0,
    listStyle: 'none',
  },
  item: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xxs,
    padding: space.xs,
    borderRadius: native.cardRadius,
    backgroundColor: native.card,
  },
  itemHead: {
    display: 'flex',
    gap: space.xs,
    alignItems: 'center',
    minWidth: 0,
    paddingInline: space.xs,
  },
  toggle: {
    ...control,
    display: 'flex',
    flexGrow: 1,
    gap: space.xs,
    alignItems: 'center',
    minWidth: 0,
    padding: 0,
    borderStyle: 'none',
    borderRadius: space.xs,
    fontFamily: 'inherit',
    fontSize: native.sectionSize,
    fontWeight: native.titleWeight,
    color: native.secondaryLabel,
    textAlign: 'start',
    appearance: 'none',
    backgroundColor: 'transparent',
  },
  chevron: {
    flexShrink: 0,
    transitionTimingFunction: easing.inOut,
    transitionDuration: { default: duration.fade, [media.reducedMotion]: '0ms' },
    transitionProperty: 'transform',
  },
  chevronOpen: { transform: 'rotate(90deg)' },
  nested: {
    paddingInlineStart: space.sm,
    borderInlineStartColor: native.separator,
    borderInlineStartStyle: 'solid',
    borderInlineStartWidth: '1px',
  },
  /** A filled, borderless text field. */
  input: {
    userSelect: 'text',
    boxSizing: 'border-box',
    width: '148px',
    paddingBlock: space.xs,
    paddingInline: space.sm,
    borderStyle: 'none',
    borderRadius: native.buttonRadius,
    fontFamily: 'inherit',
    fontSize: 'inherit',
    color: native.label,
    backgroundColor: native.control,
    outline: { default: 'none', ':focus-visible': `3px solid ${native.focus}` },
    outlineOffset: 0,
    boxShadow: `inset 0 0 0 0.5px ${native.separator}`,
    '::placeholder': { color: native.tertiaryLabel },
  },
  inputInvalid: { outline: `2px solid ${native.red}` },
  wide: { flexGrow: 1, width: 'auto', minWidth: 0 },
  message: {
    display: 'flex',
    gridColumnEnd: -1,
    gridColumnStart: 1,
    flexWrap: 'wrap',
    gap: space.sm,
    alignItems: 'center',
    margin: 0,
    fontSize: native.smallSize,
    overflowWrap: 'anywhere',
  },
  danger: { color: native.red },
  attention: { color: native.orange },
  muted: { color: native.secondaryLabel },
  actions: { display: 'flex', flexWrap: 'wrap', gap: space.sm },
});
