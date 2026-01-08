import { useCallback } from 'react';

import { useQueryClient, useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { useTauriEvent } from '@/hooks';
import { KeepAwakeEvents } from '@/types';

const fetchKeepAwake = async (): Promise<boolean> => invoke<boolean>('is_system_awake');

export const useKeepAwake = () => {
  const queryClient = useQueryClient();
  const { data: isSystemAwake } = useSuspenseQuery({
    queryKey: ['keep-awake'],
    queryFn: fetchKeepAwake,
    refetchOnMount: true,
    refetchOnWindowFocus: true,
  });

  useTauriEvent<boolean>(KeepAwakeEvents.STATE_CHANGED, ({ payload }) => {
    queryClient.setQueryData(['keep-awake'], payload);
  });

  const onKeepAwakeClick = useCallback(
    async () => queryClient.setQueryData(['keep-awake'], await invoke('toggle_system_awake')),
    [queryClient],
  );

  return { isSystemAwake, onKeepAwakeClick };
};
