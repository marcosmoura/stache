import type { RefObject } from 'react';

import type { AnyIcon } from '@/components/Icon';

export type WeatherState = {
  label: string;
  icon: AnyIcon;
  ref: RefObject<HTMLButtonElement | null>;
  onClick: () => void;
  isConfigured: boolean;
};
