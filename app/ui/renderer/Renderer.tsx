import { useMemo, useCallback, useEffect, Suspense, lazy } from 'react';

import { getCurrentWindow } from '@tauri-apps/api/window';

import { useTauriEvent } from '@/hooks';
import { AppEvents } from '@/types';

const resolveModule = (moduleName: string) => (module: Record<string, React.ComponentType>) => ({
  default: module[moduleName],
});

const Bar = lazy(() => import('./bar').then(resolveModule('Bar')));
const Widgets = lazy(() => import('./widgets').then(resolveModule('Widgets')));

export const Renderer = () => {
  const windowName = useMemo(() => getCurrentWindow().label, []);

  const onAppReload = useCallback(() => window.location.reload(), []);

  useTauriEvent(AppEvents.RELOAD, onAppReload);

  useEffect(() => console.log('App mounted for window:', windowName), [windowName]);

  return (
    <Suspense fallback={null}>
      {windowName === 'bar' && <Bar />}
      {windowName === 'widgets' && <Widgets />}
    </Suspense>
  );
};
