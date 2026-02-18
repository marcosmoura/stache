import { HugeiconsIcon } from '@hugeicons/react';

import { isSimpleIcon, type IconProps } from './Icon.types';

/**
 * Unified Icon component that automatically detects the icon library
 * - Simple Icons (IconType) are React forwardRef components
 * - HugeIcons (IconSvgElement) are SVG data objects
 */
export const Icon = ({ icon, size = 18, strokeWidth = 1.8, ...rest }: IconProps) => {
  if (isSimpleIcon(icon)) {
    const SimpleIcon = icon;
    return <SimpleIcon size={size} {...rest} />;
  }

  return <HugeiconsIcon icon={icon} size={size} strokeWidth={strokeWidth} {...rest} />;
};
