import { HugeiconsIcon } from '@hugeicons/react';

import type { IconProps, HugeIconsProps, SimpleIconsProps } from './Icon.types';

export const Icon = ({ pack = 'hugeicons', ...props }: IconProps) => {
  if (pack === 'hugeicons') {
    const { icon, size = 18, strokeWidth = 1.8, ...rest } = props as HugeIconsProps;
    return <HugeiconsIcon icon={icon} size={size} strokeWidth={strokeWidth} {...rest} />;
  }

  // simple-icons
  const { icon: SimpleIcon, size = 18, ...rest } = props as SimpleIconsProps;
  return <SimpleIcon size={size} {...rest} />;
};
