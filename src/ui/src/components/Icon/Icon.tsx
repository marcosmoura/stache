import { HugeiconsIcon, type IconSvgElement } from '@hugeicons/react';
import type { IconType } from '@icons-pack/react-simple-icons';
import * as stylex from '@stylexjs/stylex';
import type { StyleXStyles } from '@stylexjs/stylex';

import { styles } from './Icon.style';

/** The style overrides an `Icon` accepts: color, fill, size, spacing, opacity, alignment and paint transitions. */
export type IconStyle = StyleXStyles<{
  color?: string;
  opacity?: number | string;
  margin?: number | string;
  marginBlock?: number | string;
  marginInline?: number | string;
  alignSelf?: string;
  width?: number | string;
  height?: number | string;
  fill?: string;
  transitionProperty?: string;
  transitionDuration?: string;
  transitionTimingFunction?: string;
}>;

/**
 * What an `Icon` draws: a Hugeicons icon imported by name from
 * `@hugeicons/core-free-icons`, or a brand component imported by name from
 * `@icons-pack/react-simple-icons`.
 */
export type IconSource = IconSvgElement | IconType;

export type IconProps = {
  /** A Hugeicons icon or a simple-icons component. */
  icon: IconSource;
  /** Size in pixels; defaults to the `size.iconMd` token. */
  size?: number;
  /** Stroke width of a Hugeicons icon; defaults to 1.5. Simple-icons are filled and ignore it. */
  strokeWidth?: number;
  /** Accessible name. Without it the icon is hidden from assistive technology. */
  label?: string;
  /** Overrides, merged last. */
  style?: IconStyle;
};

/** Whether `icon` is a simple-icons component (Hugeicons icons are element arrays). */
function isComponent(icon: IconSource): icon is IconType {
  return !Array.isArray(icon);
}

/**
 * An icon drawn in `currentColor`: a Hugeicons icon (stroked) or a
 * simple-icons brand (filled), with the same size, accessibility and overrides.
 */
export function Icon({ icon, size, strokeWidth = 1.5, label, style }: IconProps) {
  const a11y = label
    ? ({ role: 'img', 'aria-label': label } as const)
    : ({ 'aria-hidden': true } as const);
  const styleProps = stylex.props(styles.root, size !== undefined && styles.sized(size), style);

  if (isComponent(icon)) {
    const Brand = icon;

    // simple-icons draws its own `<title>`; it carries the label or nothing.
    return (
      <Brand title={label ?? ''} color="currentColor" focusable={false} {...a11y} {...styleProps} />
    );
  }

  return (
    <HugeiconsIcon
      icon={icon}
      size={size}
      strokeWidth={strokeWidth}
      color="currentColor"
      focusable={false}
      {...a11y}
      {...styleProps}
    />
  );
}
