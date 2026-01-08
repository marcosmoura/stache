import { useMemo } from 'react';

import { colors } from '@/design-system';
import { useWidgetToggle } from '@/hooks';
import { getBatteryIcon, useBatteryStore } from '@/stores/BatteryStore';

export const useBattery = () => {
  const { ref, onClick } = useWidgetToggle('battery');

  // Get state from hook-based store (uses React Query internally)
  const { battery } = useBatteryStore();

  const { state, percentage } = battery || {};

  const label = useMemo(() => {
    if (!battery) {
      return 'Loading...';
    }

    return state === 'Full' ? '100%' : `${percentage}% (${state})`;
  }, [battery, state, percentage]);

  const icon = useMemo(() => {
    return getBatteryIcon(percentage, state);
  }, [state, percentage]);

  const color = useMemo(() => {
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
  }, [state]);

  return { percentage, label, icon, color, ref, onClick };
};
