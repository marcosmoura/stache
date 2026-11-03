import type { RefObject } from 'react';

import type { AnyIcon } from '@/components/Icon';

export type BatteryState = {
  icon: AnyIcon;
  label: string;
  color: string;
  percentage: number | undefined;
  ref: RefObject<HTMLButtonElement | null>;
  onClick: () => void;
};
