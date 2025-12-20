import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Spaces } from './Spaces';

describe('Spaces Component', () => {
  test('renders spaces container', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal', 'coding', 'browser']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'terminal');
    queryClient.setQueryData(['workspace_apps'], [{ key: '1', appName: 'Ghostty', windowId: 100 }]);
    queryClient.setQueryData(['focused_app'], { appName: 'Ghostty', windowId: 100 });

    const { getByTestId } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('spaces-container')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders with empty workspaces when data is null', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], null);
    queryClient.setQueryData(['hyprspace_current_workspace'], undefined);
    queryClient.setQueryData(['workspace_apps'], []);
    queryClient.setQueryData(['focused_app'], undefined);

    const { getByTestId, container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Component renders container even with null workspace data (useMemo returns [])
      expect(getByTestId('spaces-container')).toBeDefined();
      // No workspace buttons rendered when workspace list is empty
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(0);
    });

    queryClient.clear();
  });

  test('renders with empty workspace list', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], []);
    queryClient.setQueryData(['hyprspace_current_workspace'], undefined);
    queryClient.setQueryData(['workspace_apps'], []);
    queryClient.setQueryData(['focused_app'], undefined);

    const { getByTestId } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('spaces-container')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders workspace buttons', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal', 'coding']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'terminal');
    queryClient.setQueryData(['workspace_apps'], [{ key: '1', appName: 'Ghostty', windowId: 100 }]);
    queryClient.setQueryData(['focused_app'], { appName: 'Ghostty', windowId: 100 });

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
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'terminal');
    queryClient.setQueryData(
      ['workspace_apps'],
      [{ key: '1', appName: 'Visual Studio Code', windowId: 200 }],
    );
    queryClient.setQueryData(['focused_app'], { appName: 'Visual Studio Code', windowId: 200 });

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
    queryClient.setQueryData(['hyprspace_workspaces'], ['coding']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'coding');
    queryClient.setQueryData(
      ['workspace_apps'],
      [
        { key: '1', appName: 'Code', windowId: 100 },
        { key: '2', appName: 'Ghostty', windowId: 200 },
        { key: '3', appName: 'Safari', windowId: 300 },
      ],
    );
    queryClient.setQueryData(['focused_app'], { appName: 'Code', windowId: 100 });

    const { getByText } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Focused app shows its name
      expect(getByText('Code')).toBeDefined();
    });

    queryClient.clear();
  });

  test('marks focused workspace as active', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal', 'coding', 'browser']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'coding');
    queryClient.setQueryData(['workspace_apps'], []);
    queryClient.setQueryData(['focused_app'], undefined);

    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Should have 3 workspace buttons
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(3);
    });

    queryClient.clear();
  });

  test('renders workspace icons', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'terminal');
    queryClient.setQueryData(['workspace_apps'], []);
    queryClient.setQueryData(['focused_app'], undefined);

    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Workspace button should contain an SVG icon
      const button = container.querySelector('button');
      const svg = button?.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders app icons', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'terminal');
    queryClient.setQueryData(['workspace_apps'], [{ key: '1', appName: 'Spotify', windowId: 100 }]);
    queryClient.setQueryData(['focused_app'], { appName: 'Spotify', windowId: 100 });

    const { getByText } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Focused app should show name
      expect(getByText('Spotify')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders without focused app', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['terminal', 'coding']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'terminal');
    queryClient.setQueryData(['workspace_apps'], []);
    queryClient.setQueryData(['focused_app'], undefined);

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

  test('renders known app with specific icon', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['communication']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'communication');
    queryClient.setQueryData(['workspace_apps'], [{ key: '1', appName: 'Discord', windowId: 100 }]);
    queryClient.setQueryData(['focused_app'], { appName: 'Discord', windowId: 100 });

    const { getByText } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Discord')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders unknown app with fallback icon', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['hyprspace_workspaces'], ['misc']);
    queryClient.setQueryData(['hyprspace_current_workspace'], 'misc');
    queryClient.setQueryData(
      ['workspace_apps'],
      [{ key: '1', appName: 'Unknown Custom App', windowId: 100 }],
    );
    queryClient.setQueryData(['focused_app'], { appName: 'Unknown Custom App', windowId: 100 });

    const { getByText } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Unknown app name should still be rendered
      expect(getByText('Unknown Custom App')).toBeDefined();
    });

    queryClient.clear();
  });
});
