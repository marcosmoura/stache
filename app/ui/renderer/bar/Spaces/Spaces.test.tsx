import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Spaces } from './Spaces';

const workspaceQueryKey = ['hyprspace_workspace_data'];
const appsQueryKey = ['hyprspace_workspace_apps'];

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

const getSpacesContainer = (container: HTMLElement) =>
  container.querySelector('[data-test-id="spaces-container"]');

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

    const { getByTestId } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('spaces-container')).toBeDefined();
    });

    queryClient.clear();
  });

  test('returns null when workspace data is unavailable', async () => {
    const queryClient = createTestQueryClient();
    setWorkspaceQueryData(queryClient, null);
    setAppsQueryData(queryClient, {
      appsList: [],
    });

    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getSpacesContainer(container)).toBeNull();
    });

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

    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getSpacesContainer(container)).toBeNull();
    });

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

    await vi.waitFor(() => {
      // Should render workspace buttons with icons
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

    const { getByText } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Visual Studio Code')).toBeDefined();
    });

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

    const { getByText } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Focused app shows its name
      expect(getByText('Code')).toBeDefined();
    });

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

    const { getByTestId, container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('spaces-container')).toBeDefined();
      // Only workspace buttons, no app buttons
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(2);
    });

    queryClient.clear();
  });
});
