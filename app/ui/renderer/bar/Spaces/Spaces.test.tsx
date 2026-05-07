import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Spaces } from './Spaces';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tauri-apps/api/event')>();
  return {
    ...actual,
    listen: vi.fn().mockResolvedValue(() => {}),
    emitTo: vi.fn().mockResolvedValue(undefined),
  };
});

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((command: string) => {
    switch (command) {
      case 'is_tiling_enabled':
        return Promise.resolve(true);
      case 'get_tiling_workspaces':
        return Promise.resolve([{ name: 'terminal' }, { name: 'coding' }]);
      case 'get_tiling_focused_workspace':
        return Promise.resolve('terminal');
      case 'get_tiling_current_workspace_windows':
        return Promise.resolve([{ appName: 'Ghostty', id: 100, title: 'Ghostty' }]);
      case 'get_tiling_focused_window':
        return Promise.resolve({ appName: 'Ghostty', id: 100, title: 'Ghostty' });
      default:
        return Promise.resolve(null);
    }
  });
});

const workspaceQueryKey = ['tiling_workspace_data'];
const appsQueryKey = ['tiling_workspace_apps'];

const setWorkspaceQueryData = (
  queryClient: ReturnType<typeof createTestQueryClient>,
  data: {
    workspacesData: string[] | undefined;
    focusedWorkspace?: string;
  } | null,
) => queryClient.setQueryData(workspaceQueryKey, data);

const setAppsQueryData = (
  queryClient: ReturnType<typeof createTestQueryClient>,
  data: {
    appsList: { appName: string; windowId: number }[];
    focusedApp?: { appName: string; windowId: number };
  } | null,
) => queryClient.setQueryData(appsQueryKey, data);

describe('Spaces Component', () => {
  test('renders spaces container', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, {
      workspacesData: ['terminal', 'coding', 'browser'],
      focusedWorkspace: 'terminal',
    });
    setAppsQueryData(queryClient, {
      appsList: [{ appName: 'Ghostty', windowId: 100 }],
      focusedApp: { appName: 'Ghostty', windowId: 100 },
    });

    const screen = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();

    queryClient.clear();
  });

  test('returns null when workspace data is unavailable', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, null);
    setAppsQueryData(queryClient, {
      appsList: [],
    });

    const screen = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByTestId('spaces-container')).not.toBeInTheDocument();

    queryClient.clear();
  });

  test('returns null when workspace list is empty', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, {
      workspacesData: [],
      focusedWorkspace: undefined,
    });
    setAppsQueryData(queryClient, {
      appsList: [],
    });

    const screen = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByTestId('spaces-container')).not.toBeInTheDocument();

    queryClient.clear();
  });

  test('renders workspace buttons', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, {
      workspacesData: ['terminal', 'coding'],
      focusedWorkspace: 'terminal',
    });
    setAppsQueryData(queryClient, {
      appsList: [{ appName: 'Ghostty', windowId: 100 }],
      focusedApp: { appName: 'Ghostty', windowId: 100 },
    });

    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    // Should render workspace buttons with icons
    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBeGreaterThanOrEqual(2);
    });

    queryClient.clear();
  });

  test('renders focused app name', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, {
      workspacesData: ['terminal'],
      focusedWorkspace: 'terminal',
    });
    setAppsQueryData(queryClient, {
      appsList: [{ appName: 'Visual Studio Code', windowId: 200 }],
      focusedApp: { appName: 'Visual Studio Code', windowId: 200 },
    });

    const screen = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByText('Visual Studio Code')).toBeVisible();

    queryClient.clear();
  });

  test('renders multiple apps in workspace', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, {
      workspacesData: ['coding'],
      focusedWorkspace: 'coding',
    });
    setAppsQueryData(queryClient, {
      appsList: [
        { appName: 'Code', windowId: 100 },
        { appName: 'Ghostty', windowId: 200 },
        { appName: 'Safari', windowId: 300 },
      ],
      focusedApp: { appName: 'Code', windowId: 100 },
    });

    const screen = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    // Focused app shows its name
    await expect.element(screen.getByText('Code')).toBeVisible();

    queryClient.clear();
  });

  test('renders without focused app', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, {
      workspacesData: ['terminal', 'coding'],
      focusedWorkspace: 'terminal',
    });
    setAppsQueryData(queryClient, {
      appsList: [],
    });

    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    // Only workspace buttons, no app buttons
    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(2);
    });

    queryClient.clear();
  });
});
