import { useCallback } from 'react';

import { useQueryClient, useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { useTauriEvent } from '@/hooks';
import { KeepAwakeEvents } from '@/types';

import type { KeepAwakeState } from './KeepAwake.types';

function fetchKeepAwake(): Promise<boolean> {
  return invoke<boolean>('is_system_awake');
}

export function useKeepAwake(): KeepAwakeState {
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

  const onKeepAwakeClick = useCallback<() => Promise<void>>(async () => {
    const result = await invoke<boolean>('toggle_system_awake');
    queryClient.setQueryData(['keep-awake'], result);
  }, [queryClient]);

  return { isSystemAwake, onKeepAwakeClick };
}
