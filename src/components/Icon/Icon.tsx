import { HugeiconsIcon } from '@hugeicons/react';

import type { IconProps } from './Icon.types';

export const Icon = ({ icon, size = 18, strokeWidth = 1.8, ...rest }: IconProps) => {
  return <HugeiconsIcon icon={icon} size={size} strokeWidth={strokeWidth} {...rest} />;
};
