import type { MouseEvent, ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';
import type { StyleXStyles } from '@stylexjs/stylex';
import * as m from 'motion/react-m';

import { transitions } from '@/theme/motion';

import { styles } from './BarItem.style';

/** The overrides a `BarItem` accepts: spacing, sizing and text color. */
export type BarItemStyle = StyleXStyles<{
  color?: string;
  columnGap?: number | string;
  paddingInline?: number | string;
  paddingInlineStart?: number | string;
  minWidth?: number | string;
  maxWidth?: number | string;
  width?: number | string;
}>;

export type BarItemProps = {
  /** Makes the item a `button`; without it the item is a `div`. */
  onClick?: (event: MouseEvent<HTMLButtonElement>) => void;
  /** Shows the active (pressed) look. */
  active?: boolean;
  /** Accessible name, for items whose content is only an icon. */
  label?: string;
  children?: ReactNode;
  /** Overrides, merged last. */
  style?: BarItemStyle;
};

const FADE_IN = {
  initial: { opacity: 0 },
  animate: { opacity: 1 },
  transition: transitions.fadeIn,
};

/**
 * The rounded Bar item every widget renders into: full Bar height, a `button`
 * when clickable (rest, hover and active looks, visible keyboard focus) and a
 * `div` otherwise. It fades in once on mount (opacity only).
 */
export function BarItem({ onClick, active = false, label, children, style }: BarItemProps) {
  if (onClick) {
    return (
      <m.button
        type="button"
        aria-label={label}
        data-active={active || undefined}
        onClick={onClick}
        {...FADE_IN}
        {...stylex.props(styles.root, styles.button, active && styles.active, style)}
      >
        {children}
      </m.button>
    );
  }

  return (
    <m.div
      role={label ? 'group' : undefined}
      aria-label={label}
      data-active={active || undefined}
      {...FADE_IN}
      {...stylex.props(styles.root, active && styles.active, style)}
    >
      {children}
    </m.div>
  );
}
