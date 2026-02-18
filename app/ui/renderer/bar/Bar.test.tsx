import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';
import { MediaEvents } from '@/types';

import { Bar } from './Bar';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
  emitTo: vi.fn().mockResolvedValue(undefined),
}));

const mockInvoke = vi.mocked(invoke);

const setupQueryClient = () => {
  const queryClient = createTestQueryClient();
  queryClient.setQueryData(['tiling_workspace_data'], {
    workspacesData: ['terminal', 'coding'],
    focusedWorkspace: 'terminal',
  });
  queryClient.setQueryData(['tiling_workspace_apps'], {
    appsList: [{ appName: 'Ghostty', windowId: 100 }],
    focusedApp: { appName: 'Ghostty', windowId: 100 },
  });
  queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
    label: 'Test Song',
    prefix: '',
    bundleIdentifier: 'com.spotify.client',
    artwork: null,
  });
  return queryClient;
};

describe('Bar Component', () => {
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
        case 'get_weather_config':
          return Promise.resolve({
            provider: 'auto',
            visualCrossingApiKey: '',
            defaultLocation: 'Berlin, Germany',
          });
        default:
          return Promise.resolve(null);
      }
    });
  });

  test('renders main bar container', async () => {
    const queryClient = setupQueryClient();
    const screen = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();

    queryClient.clear();
  });

  test('renders Spaces and Status containers', async () => {
    const queryClient = setupQueryClient();
    const screen = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();
    await expect.element(screen.getByTestId('status-container')).toBeVisible();

    queryClient.clear();
  });
});
