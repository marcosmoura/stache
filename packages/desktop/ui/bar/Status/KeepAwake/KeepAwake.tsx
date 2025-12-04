import { useCallback } from 'react';

import { Coffee02Icon } from '@hugeicons/core-free-icons';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { colors } from '@/design-system';
import { useTauriEvent } from '@/hooks';

import { fetchKeepAwake, onKeepAwakeChanged, toggleKeepAwake } from './KeepAwake.service';
import * as styles from './KeepAwake.styles';

export const KeepAwake = () => {
  const queryClient = useQueryClient();
  const { data: isSystemAwake } = useQuery({
    queryKey: ['keep-awake'],
    queryFn: fetchKeepAwake,
    refetchOnMount: true,
    refetchOnWindowFocus: true,
  });

  useTauriEvent<boolean>('tauri_keep_awake_changed', ({ payload }) => {
    onKeepAwakeChanged(payload, queryClient);
  });

  const onKeepAwakeClick = useCallback(() => toggleKeepAwake(queryClient), [queryClient]);

  if (isSystemAwake === undefined) {
    return null;
  }

  return (
    <Surface
      as={Button}
      onClick={onKeepAwakeClick}
      data-test-state={isSystemAwake ? 'awake' : 'sleep'}
    >
      <Icon
        className={styles.icon}
        icon={Coffee02Icon}
        fill={isSystemAwake ? colors.green : 'transparent'}
        color={isSystemAwake ? colors.green : undefined}
      />
    </Surface>
  );
};
