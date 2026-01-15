import { useCallback, useMemo, useRef } from 'react';

import { useQueryClient, useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { useMediaQuery, useTauriEvent } from '@/hooks';
import { TilingEvents } from '@/types';
import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

import type { TilingWorkspace, TilingWindow, Workspaces } from './Spaces.types';

const workspaceOrder = [
  'terminal',
  'coding',
  'browser',
  'music',
  'design',
  'communication',
  'misc',
  'files',
  'mail',
  'tasks',
];

const emptyWorkspaces: Workspaces = [];
const emptyApps: { appName: string; windowId: number; windowTitle: string }[] = [];

const getSortedWorkspaces = (workspaces: TilingWorkspace[] | undefined) => {
  if (!workspaces) {
    return null;
  }

  return [...workspaces].sort(
    (a, b) => workspaceOrder.indexOf(a.name) - workspaceOrder.indexOf(b.name),
  );
};

const fetchWorkspacesData = async () => {
  const workspaces = await invoke<TilingWorkspace[]>('get_tiling_workspaces');
  const focusedWorkspace = await invoke<string | null>('get_tiling_focused_workspace');

  return {
    workspacesData: getSortedWorkspaces(workspaces)?.map(({ name }) => name),
    focusedWorkspace,
  };
};

const fetchAppsData = async () => {
  const windows = await invoke<TilingWindow[]>('get_tiling_current_workspace_windows');
  const focusedWindow = await invoke<TilingWindow | null>('get_tiling_focused_window');

  const apps = windows.map(({ appName, id, title }) => ({
    appName,
    windowId: id,
    windowTitle: title,
  }));

  const focusedApp = focusedWindow
    ? {
        appName: focusedWindow.appName,
        windowId: focusedWindow.id,
        windowTitle: focusedWindow.title,
      }
    : null;

  return {
    appsList: apps,
    focusedApp,
  };
};

const invokeWithErrorHandling = async <T>(
  command: string,
  args?: Record<string, unknown>,
  errorMessage?: string,
): Promise<T> => {
  try {
    const result = await invoke<T>(command, args);
    return result;
  } catch (error) {
    console.error(`${errorMessage || 'Error invoking command'} "${command}":`, error);
    throw error;
  }
};

const MAX_DISPLAY_LENGTH = 40;

const truncateText = (text: string, maxLength: number = MAX_DISPLAY_LENGTH): string => {
  if (text.length <= maxLength) {
    return text;
  }
  return `${text.slice(0, maxLength)}...`;
};

export const useSpaces = () => {
  const queryClient = useQueryClient();
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);
  const lastFocusChangedRefreshRef = useRef<Date | null>(null);

  const { data: workspaceQueryData } = useSuspenseQuery({
    queryKey: ['tiling_workspace_data'],
    queryFn: fetchWorkspacesData,
    notifyOnChangeProps: ['data'],
  });
  const { data: appQueryData } = useSuspenseQuery({
    queryKey: ['tiling_workspace_apps'],
    queryFn: fetchAppsData,
    notifyOnChangeProps: ['data'],
  });

  const workspacesData = workspaceQueryData?.workspacesData;
  const focusedWorkspace = workspaceQueryData?.focusedWorkspace;
  const focusedApp = appQueryData?.focusedApp;
  const appList = appQueryData?.appsList;

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

  const workspaces = useMemo<Workspaces>(() => {
    if (!workspacesData) {
      return emptyWorkspaces;
    }

    return workspacesData.map((name) => ({
      name,
      displayName: name.charAt(0).toUpperCase() + name.slice(1),
    }));
  }, [workspacesData]);

  const onWindowFocusChanged = useCallback(() => {
    // Debounce focus-changed events to avoid excessive refetching
    const now = new Date();

    if (
      lastFocusChangedRefreshRef.current &&
      now.getTime() - lastFocusChangedRefreshRef.current.getTime() < 200
    ) {
      return;
    }

    lastFocusChangedRefreshRef.current = now;

    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_apps'] });
  }, [queryClient]);

  const onWorkspaceChanged = useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_data'] });
    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_apps'] });
  }, [queryClient]);

  const onSpaceClick = useCallback(
    (name: string) => () =>
      invokeWithErrorHandling<void>(
        'focus_tiling_workspace',
        { name },
        'Error switching workspace',
      ),
    [],
  );

  const onAppClick = useCallback(
    (windowId: number) => () =>
      invokeWithErrorHandling<void>('focus_tiling_window', { windowId }, 'Error focusing window'),
    [],
  );

  // Listen for tiling events
  useTauriEvent(TilingEvents.WORKSPACE_WINDOWS_CHANGED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_TRACKED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_UNTRACKED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_FOCUS_CHANGED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WORKSPACE_CHANGED, onWorkspaceChanged);

  return {
    apps,
    workspaces,
    focusedWorkspace,
    focusedApp,
    onSpaceClick,
    onAppClick,
  };
};
