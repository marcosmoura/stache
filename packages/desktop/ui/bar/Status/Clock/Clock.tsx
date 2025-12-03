import { useCallback } from 'react';

import { Time03Icon } from '@hugeicons/core-free-icons';
import { useQuery } from '@tanstack/react-query';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import { getClockInfo, openClockApp } from './Clock.service';

export const Clock = () => {
  const { data: clock } = useQuery({
    queryKey: ['clock'],
    queryFn: () => getClockInfo(),
    refetchInterval: 1000,
    refetchOnMount: true,
  });

  const onClick = useCallback(() => openClockApp(), []);

  if (!clock) {
    return null;
  }

  return (
    <Surface as={Button} onClick={onClick}>
      <Icon icon={Time03Icon} />
      <span>{clock}</span>
    </Surface>
  );
};
