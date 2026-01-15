import { useCallback, useEffect, useState } from 'react';

import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

import { useTauriEvent } from '@/hooks';
import { AppEvents, TilingEvents } from '@/types';

const windowName = getCurrentWindow().label;

console.log('App mounted for window:', windowName);

export const useRenderer = () => {
  const [isReady, setIsReady] = useState(false);

  const onAppReload = useCallback(() => window.location.reload(), []);
  const onTilingInitialized = useCallback(() => setIsReady(true), []);

  // Check if tiling is already initialized on mount
  useEffect(() => {
    invoke<boolean>('is_tiling_enabled').then((enabled) => {
      if (enabled) {
        setIsReady(true);
      }
    });
  }, []);

  useTauriEvent(AppEvents.RELOAD, onAppReload);
  useTauriEvent(TilingEvents.INITIALIZED, onTilingInitialized);

  return { windowName, isReady };
};
