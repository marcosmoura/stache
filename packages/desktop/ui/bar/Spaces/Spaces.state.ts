import { useCallback, useMemo, useRef } from 'react';

import { useQueryClient, useQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { useMediaQuery, useTauriEvent } from '@/hooks';
import { CliEvents } from '@/types';
import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';
import { uuid } from '@/utils/uuid';

import type {
  HyprspaceWorkspacePayload,
  WorkspaceWindows,
  FocusedAppPayload,
  CLICommandPayload,
  Workspaces,
} from './Spaces.types';

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

const getSortedWorkspaces = (workspaces: HyprspaceWorkspacePayload[] | undefined) => {
  if (!workspaces) {
    return null;
  }

  return [...workspaces].sort(
    (a, b) => workspaceOrder.indexOf(a.workspace) - workspaceOrder.indexOf(b.workspace),
  );
};

const fetchCurrentHyprspaceWorkspace = async () => {
  const { workspace } = await invoke<HyprspaceWorkspacePayload>('get_hyprspace_focused_workspace');

  return workspace;
};

const fetchHyprspaceWorkspaceList = async () => {
  const workspaces = await invoke<HyprspaceWorkspacePayload[]>('get_hyprspace_workspaces');

  return getSortedWorkspaces(workspaces)?.map(({ workspace }) => workspace);
};

const fetchWorkspaceApps = async () => {
  const windows = await invoke<WorkspaceWindows>('get_hyprspace_current_workspace_windows');
  // const apps = new Set<string>();

  return (
    windows
      // .filter(({ appName }) => {
      //   if (apps.has(appName)) {
      //     return false;
      //   }
      //   apps.add(appName);
      //   return true;
      // })
      .map(({ appName, windowId }) => ({
        appName,
        windowId,
        key: uuid(),
      }))
  );
};

const fetchFocusedApp = async () => {
  const [{ appName, windowId }] = await invoke<FocusedAppPayload>('get_hyprspace_focused_window');

  return { appName, windowId };
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

export const useSpaces = () => {
  const queryClient = useQueryClient();
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);
  const lastFocusChangedRefreshRef = useRef<Date | null>(null);

  const { data: workspaceData } = useQuery({
    queryKey: ['hyprspace_workspaces'],
    queryFn: fetchHyprspaceWorkspaceList,
    refetchOnMount: true,
  });
  const { data: focusedWorkspace } = useQuery({
    queryKey: ['hyprspace_current_workspace'],
    queryFn: fetchCurrentHyprspaceWorkspace,
    refetchOnMount: true,
  });
  const { data: apps } = useQuery({
    queryKey: ['workspace_apps'],
    queryFn: fetchWorkspaceApps,
    refetchOnMount: true,
    enabled: !isLaptopScreen,
  });
  const { data: focusedApp } = useQuery({
    queryKey: ['focused_app'],
    queryFn: fetchFocusedApp,
    refetchOnMount: true,
  });

  useTauriEvent<CLICommandPayload>(CliEvents.COMMAND_RECEIVED, ({ payload: { name } }) => {
    // Debounce focus-changed events to avoid excessive refetching
    if (name === 'focus-changed') {
      const now = new Date();

      if (
        lastFocusChangedRefreshRef.current &&
        now.getTime() - lastFocusChangedRefreshRef.current.getTime() < 200
      ) {
        return;
      }

      lastFocusChangedRefreshRef.current = now;

      queryClient.invalidateQueries({ queryKey: ['focused_app'] });
    }

    if (name === 'workspace-changed') {
      queryClient.invalidateQueries({ queryKey: ['hyprspace_current_workspace'] });
      queryClient.invalidateQueries({ queryKey: ['hyprspace_workspaces'] });
    }

    queryClient.invalidateQueries({ queryKey: ['workspace_apps'] });
  });

  const workspaces = useMemo<Workspaces>(() => {
    if (!workspaceData) {
      return [];
    }

    return workspaceData.map((name) => ({
      name,
      displayName: name.charAt(0).toUpperCase() + name.slice(1),
      key: uuid(),
      isFocused: name === focusedWorkspace,
    }));
  }, [focusedWorkspace, workspaceData]);

  const onSpaceClick = useCallback(
    (name: string) => () =>
      invokeWithErrorHandling<void>(
        'go_to_hyprspace_workspace',
        { workspace: name },
        'Error switching workspace',
      ),
    [],
  );

  const onAppClick = useCallback(
    (windowId: number) => () =>
      invokeWithErrorHandling<void>(
        'focus_window_by_window_id',
        { windowId },
        'Error focusing app window',
      ),
    [],
  );

  return {
    apps: apps || [],
    workspaces,
    focusedWorkspace,
    focusedApp,
    isLaptopScreen,
    onSpaceClick,
    onAppClick,
  };
};
