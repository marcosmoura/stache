import { FontAwesomeIcon } from '@fortawesome/react-fontawesome';
import { HugeiconsIcon } from '@hugeicons/react';

import type { IconProps, HugeIconsProps, FontAwesomeProps } from './Icon.types';

export const Icon = ({ pack = 'hugeicons', ...props }: IconProps) => {
  if (pack === 'hugeicons') {
    const { icon, size = 18, strokeWidth = 1.8, ...rest } = props as HugeIconsProps;
    return <HugeiconsIcon icon={icon} size={size} strokeWidth={strokeWidth} {...rest} />;
  }

  return <FontAwesomeIcon {...(props as FontAwesomeProps)} />;
};
