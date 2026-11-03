import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';

import { useQueryClient, useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { useMediaQuery, useTauriEvent } from '@/hooks';
import { TilingEvents } from '@/types';
import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

import type { SpacesState, TilingWorkspace, TilingWindow, Workspaces } from './Spaces.types';

const workspaceOrder = [
  'terminal',
  'coding',
  'browser',
  'music',
  'design',
  'communication',
  'guitar',
  'misc',
  'files',
  'tasks',
];

const emptyWorkspaces: Workspaces = [];
const emptyApps: { appName: string; windowId: number; windowTitle: string }[] = [];

function getSortedWorkspaces(workspaces: TilingWorkspace[] | undefined): TilingWorkspace[] | null {
  if (!workspaces) {
    return null;
  }

  return [...workspaces].sort((a, b) => {
    const indexA = workspaceOrder.indexOf(a.name);
    const indexB = workspaceOrder.indexOf(b.name);

    if (indexA === -1 && indexB === -1) return a.name.localeCompare(b.name);
    if (indexA === -1) return 1;
    if (indexB === -1) return -1;

    return indexA - indexB;
  });
}

export async function fetchWorkspacesData() {
  try {
    const [workspaces, focusedWorkspace] = await Promise.all([
      invoke<TilingWorkspace[]>('get_tiling_workspaces'),
      invoke<string | null>('get_tiling_focused_workspace'),
    ]);

    return {
      workspacesData: getSortedWorkspaces(workspaces)?.map(({ name }) => name),
      focusedWorkspace,
    };
  } catch {
    // Tiling may not be initialized yet — return empty defaults.
    // The INITIALIZED event listener will invalidate queries once tiling is ready.
    return {
      workspacesData: undefined,
      focusedWorkspace: null,
    };
  }
}

export async function fetchAppsData() {
  try {
    const [windows, focusedWindow] = await Promise.all([
      invoke<TilingWindow[]>('get_tiling_current_workspace_windows'),
      invoke<TilingWindow | null>('get_tiling_focused_window'),
    ]);

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
  } catch {
    // Tiling may not be initialized yet — return empty defaults.
    // The INITIALIZED event listener will invalidate queries once tiling is ready.
    return { appsList: [], focusedApp: null };
  }
}

async function invokeWithErrorHandling<T>(
  command: string,
  args?: Record<string, unknown>,
  errorMessage?: string,
): Promise<T> {
  try {
    const result = await invoke<T>(command, args);
    return result;
  } catch (error) {
    console.error(`${errorMessage || 'Error invoking command'} "${command}":`, error);
    throw error;
  }
}

const MAX_DISPLAY_LENGTH = 40;

function truncateText(text: string, maxLength: number = MAX_DISPLAY_LENGTH): string {
  if (text.length <= maxLength) {
    return text;
  }

  return `${text.slice(0, maxLength)}...`;
}

export function useSpaces(): SpacesState {
  const [isEnabled, setIsEnabled] = useState(false);

  const queryClient = useQueryClient();
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);
  const focusDebounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
    clearTimeout(focusDebounceTimerRef.current || 0);
    focusDebounceTimerRef.current = setTimeout(() => {
      queryClient.invalidateQueries({ queryKey: ['tiling_workspace_apps'] });
    }, 100);
  }, [queryClient]);

  const onWorkspaceChanged = useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_data'] });
    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_apps'] });
  }, [queryClient]);

  const onSpaceClick = useCallback((name: string) => {
    void invokeWithErrorHandling<void>(
      'focus_tiling_workspace',
      { name },
      'Error switching workspace',
    );
  }, []);

  const onAppClick = useCallback((windowId: number) => {
    void invokeWithErrorHandling<void>(
      'focus_tiling_window',
      { windowId },
      'Error focusing window',
    );
  }, []);

  // Listen for tiling initialization — when tiling finishes its async startup,
  // flip isEnabled and refetch workspace data that was empty on first render.
  useTauriEvent(TilingEvents.INITIALIZED, () => {
    setIsEnabled(true);
    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_data'] });
    queryClient.invalidateQueries({ queryKey: ['tiling_workspace_apps'] });
  });

  // Listen for tiling events
  useTauriEvent(TilingEvents.WORKSPACE_WINDOWS_CHANGED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_TRACKED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_UNTRACKED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_TITLE_CHANGED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WINDOW_FOCUS_CHANGED, onWindowFocusChanged);
  useTauriEvent(TilingEvents.WORKSPACE_CHANGED, onWorkspaceChanged);

  // Fast-path: if tiling is already initialized (e.g. after a manual reload),
  // enable immediately without waiting for the INITIALIZED event.
  useLayoutEffect(() => {
    let mounted = true;

    (async () => {
      const enabled = await invoke<boolean>('is_tiling_enabled');

      if (mounted && enabled) {
        setIsEnabled(true);
      }
    })();

    return () => {
      mounted = false;
    };
  }, []);

  // Clear the focus-change debounce timer on unmount.
  useEffect(() => {
    return () => {
      clearTimeout(focusDebounceTimerRef.current || 0);
    };
  }, []);

  return {
    apps,
    workspaces,
    focusedWorkspace,
    focusedApp,
    onSpaceClick,
    onAppClick,
    isEnabled,
  };
}
