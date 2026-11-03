import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';
import { MediaEvents } from '@/types';

import { Bar } from './Bar';

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

const installDefaultInvokeMock = () => {
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
};

const setupQueryClient = (overrides?: { menuHidden?: boolean }) => {
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
  queryClient.setQueryData(['menubar-visibility'], overrides?.menuHidden ?? false);
  return queryClient;
};

describe('Bar Component', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    installDefaultInvokeMock();
  });

  test('recovers from a transient query error via the retry fallback', async () => {
    mockInvoke.mockImplementation((command: string) => {
      if (command === 'get_cpu_info') {
        return Promise.reject(new Error('transient cpu error'));
      }

      if (command === 'get_weather_config') {
        return Promise.resolve({
          provider: 'auto',
          visualCrossingApiKey: '',
          defaultLocation: 'Berlin, Germany',
        });
      }

      return null;
    });

    const screen = await render(<Bar />);

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();

    // The transient failure clears up — retry must restore the bar.
    installDefaultInvokeMock();

    await screen.getByRole('button', { name: 'Retry' }).click();

    await vi.waitFor(async () => {
      await expect.element(screen.getByTestId('status-container')).toBeVisible();
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

  test('renders correctly when menu is hidden', async () => {
    const queryClient = setupQueryClient({ menuHidden: true });
    const screen = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();

    queryClient.clear();
  });
});
