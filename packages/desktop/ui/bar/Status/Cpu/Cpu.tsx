import { useCallback } from 'react';

import { useQuery } from '@tanstack/react-query';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import { fetchCpu, getCPUElements, openActivityMonitor } from './Cpu.service';

export const Cpu = () => {
  const { data: cpu } = useQuery({
    queryKey: ['cpu'],
    queryFn: fetchCpu,
    refetchInterval: 2000, // 2 seconds
    refetchOnMount: true,
  });

  const onCpuClick = useCallback(() => openActivityMonitor(), []);

  if (!cpu) {
    return null;
  }

  const { temperature, usage } = cpu;
  const { color, icon } = getCPUElements(temperature);

  return (
    <Surface as={Button} onClick={onCpuClick}>
      <Icon icon={icon} color={color} />
      <span>{usage}%</span>
      <span>{temperature}°C</span>
    </Surface>
  );
};
