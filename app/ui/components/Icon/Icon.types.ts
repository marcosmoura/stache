import type { FontAwesomeIconProps } from '@fortawesome/react-fontawesome';
import type { HugeiconsIconProps } from '@hugeicons/react';

export type HugeIconsProps = Omit<HugeiconsIconProps, 'ref'> & {
  pack?: 'hugeicons';
};

export type FontAwesomeProps = Omit<FontAwesomeIconProps, 'ref'> & {
  pack: 'fontawesome';
};

export type IconProps = HugeIconsProps | FontAwesomeProps;
