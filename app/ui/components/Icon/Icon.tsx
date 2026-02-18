import { HugeiconsIcon } from '@hugeicons/react';
import { Icon as MdiIcon } from '@mdi/react';

import type { IconProps } from './Icon.types';
import { isMdiIcon, isSimpleIcon } from './Icon.types';

/**
 * Unified Icon component that automatically detects the icon library
 * - MDI Icons (string) are SVG path data from @mdi/js
 * - Simple Icons (IconType) are React forwardRef components
 * - HugeIcons (IconSvgElement) are SVG data objects
 */
export const Icon = ({ icon, size = 18, strokeWidth = 1.8, ...rest }: IconProps) => {
  if (isMdiIcon(icon)) {
    // eslint-disable-next-line @typescript-eslint/no-unused-vars
    const { rotate: _rotate, ...mdiRest } = rest;
    return <MdiIcon path={icon} size={`${size}px`} {...mdiRest} />;
  }

  if (isSimpleIcon(icon)) {
    const SimpleIcon = icon;
    return <SimpleIcon size={size} {...rest} />;
  }

  return <HugeiconsIcon icon={icon} size={size} strokeWidth={strokeWidth} {...rest} />;
};
