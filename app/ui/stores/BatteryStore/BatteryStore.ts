import { invoke } from '@tauri-apps/api/core';

import { useSuspenseStoreQuery } from '@/hooks';

import type { BatteryInfo } from './BatteryStore.types';

/* Constants */
const CHARGING_POLLING_INTERVAL = 30 * 1000; // 30 seconds
const DISCHARGING_POLLING_INTERVAL = 2 * 60 * 1000; // 2 minutes

const fetchBatteryInfo = () => invoke<BatteryInfo | null>('get_battery_info');

const getPollingInterval = (state?: string): number => {
  return state === 'Charging' ? CHARGING_POLLING_INTERVAL : DISCHARGING_POLLING_INTERVAL;
};

export const useBatteryStore = () => {
  const { data: battery, isLoading } = useSuspenseStoreQuery({
    queryKey: ['battery'],
    queryFn: fetchBatteryInfo,
    refetchInterval: ({ state: queryState }) => getPollingInterval(queryState.data?.state),
  });

  return {
    battery,
    isLoading,
  };
};
