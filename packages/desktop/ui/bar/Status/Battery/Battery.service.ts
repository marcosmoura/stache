import {
  BatteryEmptyIcon,
  BatteryFullIcon,
  BatteryLowIcon,
  BatteryMedium02Icon,
  BatteryMediumIcon,
  BatteryChargingIcon,
} from '@hugeicons/core-free-icons';
import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';

import type { BatteryData, BatteryInfo, BatteryState } from './Battery.types';

export const fetchBattery = async (): Promise<BatteryData | null> => {
  const battery = await invoke<BatteryInfo>('get_battery_info');

  if (!battery) {
    return null;
  }

  const { percentage, state } = battery;

  return {
    label: state === 'Full' ? '100%' : `${percentage}% (${state})`,
    percentage,
    state,
  };
};

export const getBatteryIcon = (state: BatteryState, percentage?: number) => {
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

export const getBatteryIconColor = (state: BatteryState) => {
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
};

const CHARGING_POLLING_INTERVAL = 30 * 1000; // 30 seconds
const DISCHARGING_POLLING_INTERVAL = 2 * 60 * 1000; // 2 minutes

export const getPollingInterval = (state?: BatteryState) => {
  return state === 'Charging' ? CHARGING_POLLING_INTERVAL : DISCHARGING_POLLING_INTERVAL;
};

export const openBatterySettings = () => invoke('open_app', { name: 'Battery' });
