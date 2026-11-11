import { useCallback, useMemo } from 'react';

import { useMediaQuery } from '../../../hooks';
import { LAPTOP_MEDIA_QUERY } from '../../../utils';
import { tilingClick } from '../Bar.actions';
import { useBarData } from '../Bar.data';

import type { SpacesState, WorkspaceWindows, Workspaces } from './Spaces.types';

const emptyWorkspaces: Workspaces = [];
const emptyApps: WorkspaceWindows = [];

const MAX_DISPLAY_LENGTH = 40;

function truncateText(text: string, maxLength: number = MAX_DISPLAY_LENGTH): string {
  if (text.length <= maxLength) {
    return text;
  }

  return `${text.slice(0, maxLength)}...`;
}

/**
 * Legacy `useSpaces`, reading the strip from Bar data. Updates arrive through
 * `BarChanged` (the Bar feed applies only newer sequences), so legacy's queries,
 * Tauri listeners and 100 ms refetch debounce are gone.
 */
export function useSpaces(): SpacesState {
  const { spaces } = useBarData();
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);

  const strip = spaces?.kind === 'available' ? spaces : undefined;
  const generation = strip?.generation;
  const sequence = strip?.sequence;
  const focusedWorkspace = strip?.focusedWorkspace;
  const focusedApp = strip?.focusedApp;
  const appList = strip?.apps;

  const apps = useMemo(() => {
    const baseApps = appList ?? emptyApps;
    const appsToUse = isLaptopScreen && focusedApp ? [focusedApp] : baseApps;

    // Count occurrences of each app name to identify apps with multiple windows
    const appCounts = new Map<string, number>();

    for (const app of appsToUse) {
      appCounts.set(app.appName, (appCounts.get(app.appName) ?? 0) + 1);
    }

    // Show window title for apps that have multiple windows, otherwise show app name
    return appsToUse.map((app) => ({
      ...app,
      displayName:
        (appCounts.get(app.appName) ?? 0) > 1 && app.windowTitle
          ? truncateText(app.windowTitle, isLaptopScreen ? 25 : MAX_DISPLAY_LENGTH)
          : app.appName,
    }));
  }, [isLaptopScreen, focusedApp, appList]);

  const workspaces = strip?.workspaces ?? emptyWorkspaces;

  // A click carries the generation and sequence of the strip that was rendered.
  const rendered = useMemo(
    () =>
      generation !== undefined && sequence !== undefined ? { generation, sequence } : undefined,
    [generation, sequence],
  );

  const onSpaceClick = useCallback(
    (name: string) => {
      tilingClick(rendered, { kind: 'focusWorkspace', name });
    },
    [rendered],
  );

  const onAppClick = useCallback(
    (windowId: string) => {
      tilingClick(rendered, { kind: 'focusWindow', window: windowId });
    },
    [rendered],
  );

  return {
    apps,
    workspaces,
    focusedWorkspace,
    focusedApp,
    onSpaceClick,
    onAppClick,
    isEnabled: strip !== undefined,
    isLoading: spaces?.kind === 'loading',
  };
}
