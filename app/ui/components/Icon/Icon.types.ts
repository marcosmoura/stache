import type { ComponentProps } from 'react';

import type { HugeiconsIconProps, IconSvgElement } from '@hugeicons/react';
import type { IconType } from '@icons-pack/react-simple-icons';

/**
 * Base props shared between all icon types
 */
type BaseIconProps = Omit<ComponentProps<'svg'>, 'ref'> & {
  size?: number;
};

/**
 * Props specific to HugeIcons (adds strokeWidth)
 */
type HugeIconsSpecificProps = Omit<HugeiconsIconProps, 'ref' | 'icon' | 'size'>;

/**
 * Union type for any supported icon
 * - HugeIcons: IconSvgElement (SVG data object)
 * - Simple Icons: IconType (React component)
 */
export type AnyIcon = IconSvgElement | IconType;

/**
 * Unified Icon component props
 * The component automatically detects which icon library to use based on the icon type:
 * - If icon is a function (React component) → Simple Icons
 * - If icon is an object (SVG data) → HugeIcons
 */
export type IconProps = BaseIconProps &
  HugeIconsSpecificProps & {
    icon: AnyIcon;
  };

/**
 * Type guard to check if an icon is a Simple Icon (React forwardRef component)
 * Simple Icons use React.forwardRef, which creates an object with a `render` property
 * HugeIcons are plain arrays of SVG path data
 */
export const isSimpleIcon = (icon: AnyIcon): icon is IconType => {
  return icon !== null && typeof icon === 'object' && !Array.isArray(icon) && 'render' in icon;
};
