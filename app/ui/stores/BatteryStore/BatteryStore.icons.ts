import {
  BatteryChargingIcon,
  BatteryEmptyIcon,
  BatteryFullIcon,
  BatteryLowIcon,
  BatteryMedium02Icon,
  BatteryMediumIcon,
} from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';

import type { BatteryState } from './BatteryStore.types';

export const getBatteryIcon = (
  percentage: number | undefined,
  state: BatteryState | undefined,
): IconSvgElement => {
  if (typeof percentage !== 'number') {
    return BatteryEmptyIcon;
  }

  if (state === 'Charging') {
    return BatteryChargingIcon;
  }

  switch (true) {
    case percentage === 100:
      return BatteryFullIcon;
    case percentage >= 75:
      return BatteryMedium02Icon;
    case percentage >= 50:
      return BatteryMediumIcon;
    case percentage >= 25:
      return BatteryLowIcon;
    default:
      return BatteryEmptyIcon;
  }
};
