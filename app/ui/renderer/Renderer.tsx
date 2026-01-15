import { Suspense, lazy } from 'react';

import { resolveModule } from '@/utils/resolveModule';

import { useRenderer } from './Renderer.state';

const Bar = lazy(() => import('./bar').then(resolveModule('Bar')));
const Widgets = lazy(() => import('./widgets').then(resolveModule('Widgets')));

export const Renderer = () => {
  const { windowName, isReady } = useRenderer();

  if (!isReady) {
    return null;
  }

  return (
    <Suspense fallback={null}>
      {windowName === 'bar' && <Bar />}
      {windowName === 'widgets' && <Widgets />}
    </Suspense>
  );
};
