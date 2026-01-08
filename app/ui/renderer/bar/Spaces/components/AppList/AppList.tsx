import { memo, useCallback } from 'react';

import { AnimatePresence, LayoutGroup } from 'motion/react';

import { App } from '../App';

import type { AppListProps } from './AppList.types';

export const AppList = memo(function AppList({ apps, focusedApp, onAppClick }: AppListProps) {
  const handleClick = useCallback((windowId: number) => onAppClick(windowId), [onAppClick]);

  return (
    <LayoutGroup id="apps">
      <AnimatePresence initial>
        {apps.map(({ appName, windowId, displayName }) => (
          <App
            key={windowId}
            appName={appName}
            displayName={displayName}
            windowId={windowId}
            isFocused={focusedApp?.windowId === windowId}
            onClick={handleClick(windowId)}
          />
        ))}
      </AnimatePresence>
    </LayoutGroup>
  );
});
