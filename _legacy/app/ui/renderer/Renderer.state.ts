import { useCallback } from 'react';

import { getCurrentWindow } from '@tauri-apps/api/window';

import { useTauriEvent } from '@/hooks';
import { AppEvents } from '@/types';

const windowName = getCurrentWindow().label;

console.log('App mounted for window:', windowName);

export function useRenderer() {
  const onAppReload = useCallback(() => window.location.reload(), []);

  useTauriEvent(AppEvents.RELOAD, onAppReload);

  return { windowName };
}
