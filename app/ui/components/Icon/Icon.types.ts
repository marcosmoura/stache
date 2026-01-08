import type { ComponentProps } from 'react';

import type { HugeiconsIconProps } from '@hugeicons/react';
import type { IconType } from '@icons-pack/react-simple-icons';

export type HugeIconsProps = Omit<HugeiconsIconProps, 'ref'> & {
  pack?: 'hugeicons';
};

export type SimpleIconsProps = {
  pack: 'simple-icons';
  icon: IconType;
  size?: number;
} & Omit<ComponentProps<'svg'>, 'ref'>;

export type IconProps = HugeIconsProps | SimpleIconsProps;
