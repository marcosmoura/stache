import { useCallback, useMemo, useRef } from 'react';

import { colors } from '@/design-system';
import type { WidgetConfig } from '@/renderer/widgets/Widgets.types';
import { getBatteryIcon, useBatteryStore } from '@/stores/BatteryStore';
import { WidgetsEvents } from '@/types';
import { emitTauriEvent } from '@/utils/emitTauriEvent';

export const useBattery = () => {
  const ref = useRef<HTMLButtonElement>(null);

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

  const onClick = useCallback(() => {
    if (!ref.current) {
      return;
    }

    const { x, y, width, height } = ref.current.getBoundingClientRect();

    emitTauriEvent<WidgetConfig>({
      eventName: WidgetsEvents.TOGGLE,
      target: 'widgets',
      payload: {
        name: 'battery',
        rect: {
          x,
          y,
          width,
          height,
        },
      },
    });
  }, []);

  return { percentage, label, icon, color, ref, onClick };
};
