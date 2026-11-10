import { colors } from '../../../../sdk/tokens';
import { useMediaQuery } from '../../../hooks';
import { LAPTOP_MEDIA_QUERY } from '../../../utils';
import { useBarData } from '../Bar.data';

import { getBatteryIcon } from './Battery.icons';
import type { BatteryState } from './Battery.types';

function getColor(state?: string): string {
  switch (state) {
    case 'Charging':
      return colors.green;
    case 'Discharging':
      return colors.yellow;
    case 'Empty':
      return colors.red;
    default:
      return colors.text;
  }
}

function getLabel(percentage?: number, state?: string, isCompact?: boolean): string {
  if (typeof percentage !== 'number' || !state) {
    return 'Loading...';
  }

  if (state === 'Full') {
    return '100%';
  }

  if (state === 'Unknown' || isCompact) {
    return `${percentage}%`;
  }

  return `${percentage}% (${state})`;
}

export function useBattery(): BatteryState {
  const isCompact = useMediaQuery(LAPTOP_MEDIA_QUERY);

  const { battery: reading } = useBarData();
  const { state, percentage } = reading || {};

  return {
    icon: getBatteryIcon(percentage, state),
    label: getLabel(percentage, state, isCompact),
    color: getColor(state),
    percentage,
  };
}
