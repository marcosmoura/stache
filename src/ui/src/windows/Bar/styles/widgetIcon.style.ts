import * as stylex from '@stylexjs/stylex';

import { bar, color } from '@/theme/tokens.stylex';

/** The stroke width of a Bar widget icon (today's Bar). */
export const WIDGET_ICON_STROKE = 1.8;

/** Shared Bar widget icon styles: today's size and the status tones. */
export const widgetIcon = stylex.create({
  root: {
    width: bar.iconSize,
    height: bar.iconSize,
  },
  text: { color: color.text },
  success: { color: color.success },
  warning: { color: color.warning },
  danger: { color: color.danger },
  highlight: { color: color.highlight },
});

/** A status tone a widget icon can take. */
export type IconTone = 'text' | 'success' | 'warning' | 'danger' | 'highlight';
