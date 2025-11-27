import type { QueryClient } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

export const fetchKeepAwake = async (): Promise<boolean> => {
  return await invoke<boolean>('is_system_awake');
};

export const onKeepAwakeChanged = (isAwake: boolean, queryClient: QueryClient) => {
  queryClient.setQueryData(['keep-awake'], isAwake);
};

export const toggleKeepAwake = async (queryClient: QueryClient) => {
  queryClient.setQueryData(['keep-awake'], await invoke('toggle_system_awake'));
};
