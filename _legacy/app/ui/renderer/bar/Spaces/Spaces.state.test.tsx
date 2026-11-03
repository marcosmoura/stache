import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';
import { TilingEvents } from '@/types';

import { fetchAppsData, fetchWorkspacesData, useSpaces } from './Spaces.state';
import type { TilingWindow, TilingWorkspace } from './Spaces.types';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tauri-apps/api/event')>();
  return {
    ...actual,
    listen: vi.fn((eventName: string, callback: (event: { payload: unknown }) => void) => {
      listeners.set(eventName, callback);
      return Promise.resolve(() => {
        listeners.delete(eventName);
      });
    }),
    emitTo: vi.fn(() => Promise.resolve()),
  };
});

const listeners = new Map<string, (event: { payload: unknown }) => void>();
const mockInvoke = vi.mocked(invoke);

const workspaces: TilingWorkspace[] = [
  {
    name: 'terminal',
    screenId: 1,
    screenName: 'Built-in',
    layout: 'dwindle',
    isVisible: true,
    isFocused: true,
    windowCount: 1,
    windowIds: [1],
  },
  {
    name: 'coding',
    screenId: 1,
    screenName: 'Built-in',
    layout: 'dwindle',
    isVisible: true,
    isFocused: false,
    windowCount: 0,
    windowIds: [],
  },
];

const windows: TilingWindow[] = [
  {
    id: 1,
    pid: 1,
    appId: 'app',
    appName: 'Ghostty',
    title: 'zsh',
    workspace: 'terminal',
    isFocused: true,
  },
];

const seedQueryData = (queryClient: ReturnType<typeof createTestQueryClient>) => {
  queryClient.setQueryData(['tiling_workspace_data'], {
    workspacesData: ['terminal', 'coding'],
    focusedWorkspace: 'terminal',
  });
  queryClient.setQueryData(['tiling_workspace_apps'], {
    appsList: [{ appName: 'Ghostty', windowId: 1, windowTitle: 'zsh' }],
    focusedApp: { appName: 'Ghostty', windowId: 1, windowTitle: 'zsh' },
  });
};

describe('fetchWorkspacesData', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(null);
  });

  test('fetches workspaces and the focused workspace in parallel', async () => {
    let resolveWorkspaces: ((value: TilingWorkspace[]) => void) | undefined;
    let resolveFocused: ((value: string | null) => void) | undefined;

    mockInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_tiling_workspaces') {
        return new Promise<TilingWorkspace[]>((resolve) => {
          resolveWorkspaces = resolve;
        });
      }
      if (command === 'get_tiling_focused_workspace') {
        return new Promise<string | null>((resolve) => {
          resolveFocused = resolve;
        });
      }
      return null;
    });

    const resultPromise = fetchWorkspacesData();

    // Both invokes must be dispatched before either resolves.
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('get_tiling_workspaces');
      expect(mockInvoke).toHaveBeenCalledWith('get_tiling_focused_workspace');
    });

    resolveWorkspaces?.(workspaces);
    resolveFocused?.('terminal');

    await expect(resultPromise).resolves.toEqual({
      workspacesData: ['terminal', 'coding'],
      focusedWorkspace: 'terminal',
    });
  });

  test('returns empty defaults when tiling is unavailable', async () => {
    mockInvoke.mockRejectedValue(new Error('tiling not ready'));

    await expect(fetchWorkspacesData()).resolves.toEqual({
      workspacesData: undefined,
      focusedWorkspace: null,
    });
  });
});

describe('fetchAppsData', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(null);
  });

  test('fetches windows and the focused window in parallel', async () => {
    let resolveWindows: ((value: TilingWindow[]) => void) | undefined;
    let resolveFocused: ((value: TilingWindow | null) => void) | undefined;

    mockInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_tiling_current_workspace_windows') {
        return new Promise<TilingWindow[]>((resolve) => {
          resolveWindows = resolve;
        });
      }
      if (command === 'get_tiling_focused_window') {
        return new Promise<TilingWindow | null>((resolve) => {
          resolveFocused = resolve;
        });
      }
      return null;
    });

    const resultPromise = fetchAppsData();

    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('get_tiling_current_workspace_windows');
      expect(mockInvoke).toHaveBeenCalledWith('get_tiling_focused_window');
    });

    resolveWindows?.(windows);
    resolveFocused?.(windows[0]);

    await expect(resultPromise).resolves.toEqual({
      appsList: [{ appName: 'Ghostty', windowId: 1, windowTitle: 'zsh' }],
      focusedApp: { appName: 'Ghostty', windowId: 1, windowTitle: 'zsh' },
    });
  });

  test('returns empty defaults when tiling is unavailable', async () => {
    mockInvoke.mockRejectedValue(new Error('tiling not ready'));

    await expect(fetchAppsData()).resolves.toEqual({
      appsList: [],
      focusedApp: null,
    });
  });
});

describe('useSpaces', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    listeners.clear();

    mockInvoke.mockImplementation(async (command: string) => {
      switch (command) {
        case 'is_tiling_enabled':
          return true;
        case 'get_tiling_workspaces':
          return workspaces;
        case 'get_tiling_focused_workspace':
          return 'terminal';
        case 'get_tiling_current_workspace_windows':
          return windows;
        case 'get_tiling_focused_window':
          return windows[0];
        default:
          return null;
      }
    });
  });

  test('focuses the selected workspace', async () => {
    const queryClient = createTestQueryClient();
    seedQueryData(queryClient);
    const { result, unmount } = await renderHook(() => useSpaces(), {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(result.current.isEnabled).toBe(true);
    });

    result.current.onSpaceClick('coding');

    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('focus_tiling_workspace', { name: 'coding' });
    });

    await unmount();
    queryClient.clear();
  });

  test('focuses the selected app window', async () => {
    const queryClient = createTestQueryClient();
    seedQueryData(queryClient);
    const { result, unmount } = await renderHook(() => useSpaces(), {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(result.current.isEnabled).toBe(true);
    });

    result.current.onAppClick(7);

    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('focus_tiling_window', { windowId: 7 });
    });

    await unmount();
    queryClient.clear();
  });

  test('does not fire the focus-change debounce after unmount', async () => {
    const queryClient = createTestQueryClient();
    seedQueryData(queryClient);
    const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries');

    const { unmount } = await renderHook(() => useSpaces(), {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(listeners.has(TilingEvents.WINDOW_FOCUS_CHANGED)).toBe(true);
    });

    const focusChanged = listeners.get(TilingEvents.WINDOW_FOCUS_CHANGED);
    focusChanged?.({ payload: {} });

    await unmount();

    // Allow the 100ms debounce window to pass — no invalidation should occur.
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(invalidateSpy).not.toHaveBeenCalled();

    queryClient.clear();
  });

  test('invalidates the apps query after the focus-change debounce', async () => {
    const queryClient = createTestQueryClient();
    seedQueryData(queryClient);
    const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries');

    const { unmount } = await renderHook(() => useSpaces(), {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(listeners.has(TilingEvents.WINDOW_FOCUS_CHANGED)).toBe(true);
    });

    const focusChanged = listeners.get(TilingEvents.WINDOW_FOCUS_CHANGED);
    focusChanged?.({ payload: {} });

    await vi.waitFor(() => {
      expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: ['tiling_workspace_apps'] });
    });

    await unmount();
    queryClient.clear();
  });
});
