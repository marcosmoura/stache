import * as stylex from '@stylexjs/stylex';

import { color, duration, easing } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  icon: {
    color: color.text,
    fill: 'transparent',
    transitionTimingFunction: easing.inOut,
    transitionDuration: duration.bar,
    transitionProperty: 'color, fill',
  },
  awake: {
    color: color.success,
    fill: 'currentColor',
  },
});
