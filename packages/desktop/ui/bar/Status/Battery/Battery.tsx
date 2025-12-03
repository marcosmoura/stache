import { useCallback } from 'react';

import { useQuery } from '@tanstack/react-query';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import {
  fetchBattery,
  getBatteryIcon,
  getBatteryIconColor,
  getPollingInterval,
  openBatterySettings,
} from './Battery.service';

export const Battery = () => {
  const { data: battery } = useQuery({
    queryKey: ['battery'],
    queryFn: fetchBattery,
    refetchInterval: ({ state }) => getPollingInterval(state.data?.state),
    refetchOnMount: true,
  });

  const onBatteryClick = useCallback(() => openBatterySettings(), []);

  if (!battery) {
    return null;
  }

  const { state, percentage, label } = battery;

  return (
    <Surface as={Button} onClick={onBatteryClick}>
      <Icon icon={getBatteryIcon(state, percentage)} color={getBatteryIconColor(state)} />
      <span>{label}</span>
    </Surface>
  );
};
